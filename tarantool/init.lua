-- ChainDeal storage node.
-- Holds the block chain, transaction log, mempool and the materialised world
-- state (accounts + deals). All state changes produced by one block are
-- committed in a single Tarantool transaction by cd_commit_block().

local log = require('log')
local clock = require('clock')

local DB_USER = os.getenv('CHAINDEAL_DB_USER') or 'chaindeal'
local DB_PASSWORD = os.getenv('CHAINDEAL_DB_PASSWORD') or 'chaindeal'

-- Topology: a single writable master, optionally followed by read-only
-- replicas. A replica is started with TT_REPLICATION_SOURCE pointing at the
-- master (e.g. "tarantool-0.tarantool:3301"); it joins, streams the WAL and
-- serves reads. Without it the instance is a standalone master.
local source = os.getenv('TT_REPLICATION_SOURCE')
if source == '' then source = nil end

box.cfg {
    listen = os.getenv('TT_LISTEN') or 3301,
    work_dir = os.getenv('TT_WORK_DIR') or '/var/lib/tarantool',
    -- 1M+ transactions need a few GB; see docker-compose.yml / deploy/k8s.
    memtx_memory = tonumber(os.getenv('TT_MEMTX_MEMORY_MB') or 1024) * 1024 * 1024,
    log_level = 5,
    replication = source and { string.format('%s:%s@%s', DB_USER, DB_PASSWORD, source) } or nil,
    read_only = source ~= nil,
    replication_connect_timeout = 60,
}

-- Schema changes only ever run on the master; replicas receive them via replication.
local IS_MASTER = not box.info.ro
local function once(name, fn)
    if IS_MASTER then box.once(name, fn) end
end

once('chaindeal-schema-v1', function()
    local accounts = box.schema.space.create('accounts', {
        format = {
            { name = 'address', type = 'string' },
            { name = 'doc', type = 'map' },
        },
    })
    accounts:create_index('primary', { parts = { 'address' } })

    local deals = box.schema.space.create('deals', {
        format = {
            { name = 'id', type = 'string' },
            { name = 'seller', type = 'string' },
            { name = 'buyer', type = 'string' },
            { name = 'arbiter', type = 'string', is_nullable = true },
            { name = 'status', type = 'string' },
            { name = 'updated_at', type = 'unsigned' },
            { name = 'doc', type = 'map' },
        },
    })
    deals:create_index('primary', { parts = { 'id' } })
    deals:create_index('seller', { parts = { 'seller' }, unique = false })
    deals:create_index('buyer', { parts = { 'buyer' }, unique = false })
    deals:create_index('arbiter', { parts = { { field = 'arbiter', is_nullable = true } }, unique = false })
    deals:create_index('updated', { parts = { 'updated_at', 'id' } })

    local blocks = box.schema.space.create('blocks', {
        format = {
            { name = 'height', type = 'unsigned' },
            { name = 'hash', type = 'string' },
            { name = 'doc', type = 'map' },
        },
    })
    blocks:create_index('primary', { parts = { 'height' } })
    blocks:create_index('hash', { parts = { 'hash' } })

    box.schema.sequence.create('tx_seq')
    local txs = box.schema.space.create('txs', {
        format = {
            { name = 'hash', type = 'string' },
            { name = 'seq', type = 'unsigned' },
            { name = 'block', type = 'unsigned', is_nullable = true },
            { name = 'from', type = 'string' },
            { name = 'status', type = 'string' },
            { name = 'doc', type = 'map' },
        },
    })
    txs:create_index('primary', { parts = { 'hash' } })
    txs:create_index('seq', { parts = { 'seq' } })
    txs:create_index('from', { parts = { 'from', 'seq' } })
    txs:create_index('block', { parts = { { field = 'block', is_nullable = true }, 'seq' }, unique = false })

    local mempool = box.schema.space.create('mempool', {
        format = {
            { name = 'seq', type = 'unsigned' },
            { name = 'hash', type = 'string' },
            { name = 'doc', type = 'map' },
        },
    })
    mempool:create_index('primary', { parts = { 'seq' }, sequence = true })
    mempool:create_index('hash', { parts = { 'hash' } })

    box.schema.user.create(DB_USER, { password = DB_PASSWORD, if_not_exists = true })
    box.schema.user.grant(DB_USER, 'read,write,execute', 'universe', nil, { if_not_exists = true })
    log.info('chaindeal schema created')
end)

-- v2: incrementally maintained statistics so dashboards never scan 1M rows.
once('chaindeal-schema-v2', function()
    local c = box.schema.space.create('counters', {
        format = { { name = 'key', type = 'string' }, { name = 'value', type = 'integer' } },
    })
    c:create_index('primary', { parts = { 'key' } })
    -- Backfill from any existing deals.
    for _, t in box.space.deals:pairs() do
        local d = t.doc
        c:upsert({ 'status:' .. d.status, 1 }, { { '+', 2, 1 } })
        c:upsert({ 'type:' .. d.deal_type, 1 }, { { '+', 2, 1 } })
        c:upsert({ 'volume', d.amount }, { { '+', 2, d.amount } })
        if d.status == 'completed' or d.status == 'resolved' then
            c:upsert({ 'settled', d.amount }, { { '+', 2, d.amount } })
        end
    end
end)

-- v3: status index, so filtered and "open" deal lists are index range reads
-- instead of scans over hundreds of thousands of deals.
once('chaindeal-schema-v3', function()
    box.space.deals:create_index('status', { parts = { 'status', 'updated_at', 'id' } })
end)

local function bump(key, delta)
    if delta ~= 0 then
        box.space.counters:upsert({ key, delta }, { { '+', 2, delta } })
    end
end

-- v4: cluster coordination. Several stateless nodes share one master:
--   leases   leader election (who mines blocks and runs the simulator)
--   events   the live event log every node tails for its SSE clients
--   metrics  per-second throughput counters aggregated across nodes
--   kv       shared settings and snapshots (simulator config/state)
--   nodes    heartbeats, for the cluster view
once('chaindeal-schema-v4', function()
    local leases = box.schema.space.create('leases', {
        format = { { name = 'name', type = 'string' }, { name = 'holder', type = 'string' }, { name = 'expires', type = 'number' } },
    })
    leases:create_index('primary', { parts = { 'name' } })

    box.schema.sequence.create('event_seq')
    local events = box.schema.space.create('events', {
        format = { { name = 'seq', type = 'unsigned' }, { name = 'at', type = 'number' }, { name = 'data', type = 'string' } },
    })
    events:create_index('primary', { parts = { 'seq' }, sequence = 'event_seq' })

    local metrics = box.schema.space.create('metrics', {
        format = {
            { name = 'sec', type = 'unsigned' }, { name = 'admitted', type = 'unsigned' }, { name = 'refused', type = 'unsigned' },
            { name = 'confirmed', type = 'unsigned' }, { name = 'rejected', type = 'unsigned' },
        },
    })
    metrics:create_index('primary', { parts = { 'sec' } })

    local kv = box.schema.space.create('kv', {
        format = { { name = 'key', type = 'string' }, { name = 'value', type = 'string' } },
    })
    kv:create_index('primary', { parts = { 'key' } })

    local nodes = box.schema.space.create('nodes', {
        format = { { name = 'id', type = 'string' }, { name = 'last_seen', type = 'number' }, { name = 'info', type = 'string' } },
    })
    nodes:create_index('primary', { parts = { 'id' } })

    box.schema.user.grant(DB_USER, 'replication', nil, nil, { if_not_exists = true })
end)

-- Keep the password in sync with the environment on restarts.
if IS_MASTER then box.schema.user.passwd(DB_USER, DB_PASSWORD) end

local function docs(iter_space, index, key, opts, limit)
    local out = {}
    for _, t in box.space[iter_space].index[index]:pairs(key, opts) do
        if limit and #out >= limit then break end
        table.insert(out, t.doc)
    end
    return out
end

---------------------------------------------------------------- reads

function cd_tip()
    local t = box.space.blocks.index.primary:max()
    return t and t.doc or box.NULL
end

function cd_get_accounts(addrs)
    local out = {}
    for _, a in ipairs(addrs) do
        local t = box.space.accounts:get(a)
        if t then table.insert(out, t.doc) end
    end
    return out
end

function cd_list_accounts(limit)
    return docs('accounts', 'primary', nil, { iterator = 'ALL' }, limit or 500)
end

-- Directory search: substring on name or address prefix, optional kind filter,
-- ordered by settled deals. A full scan, which is fine up to ~100k accounts.
function cd_search_accounts(q, kind, offset, limit)
    q = (q ~= nil and q ~= box.NULL) and string.lower(q) or ''
    if kind == box.NULL then kind = nil end
    local hits = {}
    for _, t in box.space.accounts:pairs() do
        local d = t.doc
        if d.pubkey ~= '' and (kind == nil or d.kind == kind)
            and (q == '' or string.find(string.lower(d.name), q, 1, true) or string.sub(d.address, 1, #q) == q) then
            table.insert(hits, d)
        end
    end
    table.sort(hits, function(a, b)
        if a.deals_completed ~= b.deals_completed then return a.deals_completed > b.deals_completed end
        return a.name < b.name
    end)
    local out = {}
    for i = (offset or 0) + 1, math.min(#hits, (offset or 0) + (limit or 50)) do
        table.insert(out, hits[i])
    end
    return { total = #hits, items = out }
end

function cd_get_deals(ids)
    local out = {}
    for _, id in ipairs(ids) do
        local t = box.space.deals:get(id)
        if t then table.insert(out, t.doc) end
    end
    return out
end

local TERMINAL = { completed = true, resolved = true, cancelled = true, declined = true, expired = true, failed = true }

local OPEN = { 'proposed', 'accepted', 'funded', 'shipped', 'disputed' }

-- Newest-first deals with optional type/status filters ('open' = any non-terminal).
-- Status filters walk the (status, updated_at) index; only the type filter scans.
function cd_list_deals(limit, offset, dtype, status)
    if dtype == box.NULL then dtype = nil end
    if status == box.NULL then status = nil end
    limit, offset = limit or 100, offset or 0
    local want = offset + limit
    local rows = {}
    if status == nil then
        for _, t in box.space.deals.index.updated:pairs(nil, { iterator = 'REQ' }) do
            if dtype == nil or t.doc.deal_type == dtype then
                table.insert(rows, t)
                if #rows >= want then break end
            end
        end
    else
        local statuses = status == 'open' and OPEN or { status }
        for _, st in ipairs(statuses) do
            local n = 0
            for _, t in box.space.deals.index.status:pairs({ st }, { iterator = 'REQ' }) do
                if dtype == nil or t.doc.deal_type == dtype then
                    table.insert(rows, t)
                    n = n + 1
                    if n >= want then break end
                end
            end
        end
        table.sort(rows, function(a, b)
            if a.updated_at ~= b.updated_at then return a.updated_at > b.updated_at end
            return a.id > b.id
        end)
    end
    local out = {}
    for k = offset + 1, math.min(#rows, want) do table.insert(out, rows[k].doc) end
    return out
end

function cd_deals_for(addr, limit)
    local seen, out = {}, {}
    for _, idx in ipairs({ 'seller', 'buyer', 'arbiter' }) do
        for _, t in box.space.deals.index[idx]:pairs(addr) do
            if not seen[t.id] then
                seen[t.id] = true
                table.insert(out, t)
            end
        end
    end
    table.sort(out, function(a, b) return a.updated_at > b.updated_at end)
    local res = {}
    for i, t in ipairs(out) do
        if limit and i > limit then break end
        table.insert(res, t.doc)
    end
    return res
end

function cd_list_blocks(before, limit)
    if before == nil or before == box.NULL then
        return docs('blocks', 'primary', nil, { iterator = 'REQ' }, limit)
    end
    return docs('blocks', 'primary', before, { iterator = 'LT' }, limit)
end

local function block_with_txs(t)
    if t == nil then return box.NULL end
    return { block = t.doc, txs = docs('txs', 'block', t.height, { iterator = 'EQ' }) }
end

function cd_get_block(key)
    if type(key) == 'number' then
        return block_with_txs(box.space.blocks:get(key))
    end
    return block_with_txs(box.space.blocks.index.hash:get(key))
end

-- Batch of blocks with their transactions, for full-chain verification.
function cd_blocks_range(from, limit)
    local out = {}
    for _, t in box.space.blocks.index.primary:pairs(from, { iterator = 'GE' }) do
        if #out >= limit then break end
        table.insert(out, block_with_txs(t))
    end
    return out
end

function cd_get_tx(hash)
    local t = box.space.txs:get(hash)
    if t then return t.doc end
    local m = box.space.mempool.index.hash:get(hash)
    if m then return m.doc end
    return box.NULL
end

function cd_recent_txs(limit)
    return docs('txs', 'seq', nil, { iterator = 'REQ' }, limit)
end

function cd_account_txs(addr, limit)
    return docs('txs', 'from', { addr }, { iterator = 'REQ' }, limit)
end

function cd_stats()
    local by_status, by_type = {}, {}
    local volume, settled = 0, 0
    for _, t in box.space.counters:pairs() do
        local k, v = t.key, t.value
        if string.sub(k, 1, 7) == 'status:' then
            by_status[string.sub(k, 8)] = v
        elseif string.sub(k, 1, 5) == 'type:' then
            by_type[string.sub(k, 6)] = v
        elseif k == 'volume' then
            volume = v
        elseif k == 'settled' then
            settled = v
        end
    end
    local tip = box.space.blocks.index.primary:max()
    return {
        height = tip and tip.height or 0,
        tip_hash = tip and tip.hash or box.NULL,
        tip_time = tip and tip.doc.header.timestamp or 0,
        accounts = box.space.accounts:len(),
        deals = box.space.deals:len(),
        txs = box.space.txs:len(),
        mempool = box.space.mempool:len(),
        deals_by_status = setmetatable(by_status, { __serialize = 'map' }),
        deals_by_type = setmetatable(by_type, { __serialize = 'map' }),
        volume = volume,
        settled_volume = settled,
    }
end

---------------------------------------------------------------- writes

function cd_mempool_add(tx)
    if box.space.txs:get(tx.hash) or box.space.mempool.index.hash:get(tx.hash) then
        box.error({ reason = 'duplicate transaction ' .. tx.hash })
    end
    box.space.mempool:insert({ box.NULL, tx.hash, tx })
    return true
end

function cd_mempool_take(limit)
    return docs('mempool', 'primary', nil, { iterator = 'GE' }, limit)
end

-- Atomically appends a block and applies everything it changed.
-- txs: confirmed + rejected transaction records (rejected carry block = nil).
-- block may be nil when a batch contained only rejected transactions.
function cd_commit_block(block, txs, accounts, deals)
    box.atomic(function()
        if block ~= nil then
            local h = block.header.height
            local tip = box.space.blocks.index.primary:max()
            if tip then
                if h ~= tip.height + 1 or block.header.prev_hash ~= tip.hash then
                    box.error({ reason = string.format('block %d does not extend tip %d', h, tip.height) })
                end
            elseif h ~= 0 then
                box.error({ reason = 'first block must be genesis' })
            end
            box.space.blocks:insert({ h, block.hash, block })
        end

        for _, tx in ipairs(txs) do
            local bh = tx.block_height
            if bh == nil then bh = box.NULL end
            box.space.txs:replace({ tx.hash, box.sequence.tx_seq:next(), bh, tx.body.from, tx.status, tx })
            box.space.mempool.index.hash:delete(tx.hash)
        end
        for _, a in ipairs(accounts) do
            box.space.accounts:replace({ a.address, a })
        end
        for _, d in ipairs(deals) do
            local old = box.space.deals:get(d.id)
            if old == nil then
                bump('type:' .. d.deal_type, 1)
                bump('volume', d.amount)
                bump('status:' .. d.status, 1)
            elseif old.status ~= d.status then
                bump('status:' .. old.status, -1)
                bump('status:' .. d.status, 1)
            end
            if (d.status == 'completed' or d.status == 'resolved') and (old == nil or old.status ~= d.status) then
                bump('settled', d.amount)
            end
            box.space.deals:replace({ d.id, d.seller, d.buyer, d.arbiter, d.status, d.updated_at, d })
        end
    end)
    return true
end

---------------------------------------------------------------- cluster

-- Acquire or renew a named lease. Returns true if `holder` owns it now.
function cd_lease(name, holder, ttl)
    local now = clock.time()
    local t = box.space.leases:get(name)
    if t == nil or t.holder == holder or t.expires < now then
        box.space.leases:replace({ name, holder, now + ttl })
        return true
    end
    return false
end

function cd_lease_holder(name)
    local t = box.space.leases:get(name)
    if t == nil or t.expires < clock.time() then return box.NULL end
    return t.holder
end

-- Append serialized events (JSON strings) to the shared log.
function cd_publish(items)
    local now = clock.time()
    box.atomic(function()
        for _, data in ipairs(items) do
            box.space.events:insert({ box.NULL, now, data })
        end
    end)
    return true
end

function cd_events_head()
    local t = box.space.events.index.primary:max()
    return t and t.seq or 0
end

function cd_events_since(seq, limit)
    local out = {}
    for _, t in box.space.events.index.primary:pairs(seq, { iterator = 'GT' }) do
        if #out >= limit then break end
        table.insert(out, { t.seq, t.data })
    end
    return out
end

-- rows: { {sec, admitted, refused, confirmed, rejected}, ... }
function cd_metric_add(rows)
    box.atomic(function()
        for _, r in ipairs(rows) do
            box.space.metrics:upsert(r, { { '+', 2, r[2] }, { '+', 3, r[3] }, { '+', 4, r[4] }, { '+', 5, r[5] } })
        end
    end)
    return true
end

function cd_metrics(from_sec)
    local out = {}
    for _, t in box.space.metrics.index.primary:pairs(from_sec, { iterator = 'GE' }) do
        table.insert(out, { t.sec, t.admitted, t.refused, t.confirmed, t.rejected })
    end
    return out
end

-- Drop event-log entries and metrics older than `max_age` seconds.
function cd_prune(max_age)
    local cutoff = clock.time() - max_age
    local n = 0
    for _, t in box.space.events.index.primary:pairs() do
        if t.at >= cutoff or n >= 5000 then break end
        box.space.events:delete(t.seq)
        n = n + 1
    end
    for _, t in box.space.metrics.index.primary:pairs() do
        if t.sec >= cutoff or n >= 10000 then break end
        box.space.metrics:delete(t.sec)
        n = n + 1
    end
    return n
end

function cd_kv_get(key)
    local t = box.space.kv:get(key)
    return t and t.value or box.NULL
end

function cd_kv_set(key, value)
    box.space.kv:replace({ key, value })
    return true
end

function cd_heartbeat(id, info)
    box.space.nodes:replace({ id, clock.time(), info })
    return true
end

-- Nodes seen within `max_age` seconds, with seconds since last heartbeat.
function cd_nodes(max_age)
    local now, out = clock.time(), {}
    for _, t in box.space.nodes:pairs() do
        if now - t.last_seen <= max_age then
            table.insert(out, { id = t.id, age = now - t.last_seen, info = t.info })
        elseif now - t.last_seen > 3600 then
            if IS_MASTER then box.space.nodes:delete(t.id) end
        end
    end
    return out
end

-- This instance's view of the replica set, for the cluster page.
function cd_instance_info()
    local peers = {}
    for _, r in pairs(box.info.replication) do
        table.insert(peers, {
            id = r.id,
            uuid = r.uuid,
            lsn = r.lsn,
            upstream = r.upstream and { status = r.upstream.status, lag = r.upstream.lag } or box.NULL,
            downstream = r.downstream and { status = r.downstream.status, lag = r.downstream.lag } or box.NULL,
        })
    end
    return {
        id = box.info.id,
        uuid = box.info.uuid,
        ro = box.info.ro,
        status = box.info.status,
        lsn = box.info.lsn,
        uptime = box.info.uptime,
        hostname = os.getenv('HOSTNAME') or 'tarantool',
        memory_used = box.slab.info().arena_used,
        memory_limit = box.slab.info().quota_size,
        txs = box.space.txs and box.space.txs:len() or 0,
        replication = peers,
    }
end

log.info('chaindeal storage ready (%s)', IS_MASTER and 'master' or 'read-only replica')
