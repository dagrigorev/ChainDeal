-- ChainDeal auth database. Owned exclusively by the auth service (only its
-- pods can reach it). Secrets are never stored in the clear: passwords are
-- Argon2id PHC strings, emails are AES-256-GCM ciphertext looked up through an
-- HMAC blind index, and codes / refresh tokens / sessions are SHA-256 hashes.

local log = require('log')
local fiber = require('fiber')
local clock = require('clock')

local DB_USER = os.getenv('AUTH_DB_USER') or 'auth'
local DB_PASSWORD = os.getenv('AUTH_DB_PASSWORD') or 'auth'

box.cfg {
    listen = os.getenv('TT_LISTEN') or 3301,
    work_dir = os.getenv('TT_WORK_DIR') or '/var/lib/tarantool',
    memtx_memory = tonumber(os.getenv('TT_MEMTX_MEMORY_MB') or 128) * 1024 * 1024,
    log_level = 5,
}

-- Every entity space: {key, idx, expires, doc}. `idx` is the one secondary
-- lookup (email index, owner, token family); `expires` = 0 means never.
local ENTITY = { 'users', 'wallets', 'codes', 'refresh', 'sessions', 'challenges' }

box.once('auth-schema-v1', function()
    for _, name in ipairs(ENTITY) do
        local s = box.schema.space.create(name, {
            format = {
                { name = 'key', type = 'string' },
                { name = 'idx', type = 'string' },
                { name = 'expires', type = 'number' },
                { name = 'doc', type = 'map' },
            },
        })
        s:create_index('primary', { parts = { 'key' } })
        -- One account per email: the blind index is unique for users.
        s:create_index('idx', { parts = { 'idx', 'key' }, unique = true })
        s:create_index('expires', { parts = { 'expires', 'key' } })
    end
    box.space.users.index.idx:drop()
    box.space.users:create_index('idx', { parts = { 'idx' }, unique = true })

    box.schema.sequence.create('audit_seq')
    local audit = box.schema.space.create('audit', {
        format = { { name = 'seq', type = 'unsigned' }, { name = 'at', type = 'number' }, { name = 'doc', type = 'map' } },
    })
    audit:create_index('primary', { parts = { 'seq' }, sequence = 'audit_seq' })

    box.schema.user.create(DB_USER, { password = DB_PASSWORD, if_not_exists = true })
    box.schema.user.grant(DB_USER, 'read,write,execute', 'universe', nil, { if_not_exists = true })
    log.info('auth schema created')
end)
box.schema.user.passwd(DB_USER, DB_PASSWORD)

local function live(t, now)
    return t ~= nil and (t.expires == 0 or t.expires > now)
end

function auth_get(space, key)
    local t = box.space[space]:get(key)
    if live(t, clock.time()) then return t.doc end
    return box.NULL
end

function auth_put(space, key, idx, expires, doc)
    box.space[space]:replace({ key, idx, expires, doc })
    return true
end

-- Insert that reports a uniqueness conflict instead of raising.
function auth_insert(space, key, idx, expires, doc)
    local ok, err = pcall(box.space[space].insert, box.space[space], { key, idx, expires, doc })
    if ok then return true end
    if tostring(err):find('Duplicate') then return false end
    error(err)
end

function auth_delete(space, key)
    box.space[space]:delete(key)
    return true
end

function auth_by_idx(space, idx, limit)
    local now, out = clock.time(), {}
    for _, t in box.space[space].index.idx:pairs({ idx }) do
        if #out >= (limit or 100) then break end
        if live(t, now) then table.insert(out, t.doc) end
    end
    return out
end

function auth_by_idx_one(space, idx)
    local t = box.space[space].index.idx:get({ idx })
    if live(t, clock.time()) then return t.doc end
    return box.NULL
end

function auth_delete_by_idx(space, idx)
    local keys = {}
    for _, t in box.space[space].index.idx:pairs({ idx }) do table.insert(keys, t.key) end
    box.atomic(function()
        for _, k in ipairs(keys) do box.space[space]:delete(k) end
    end)
    return #keys
end

-- Single-use read: returns the document and deletes it atomically.
function auth_consume(space, key)
    local out = box.NULL
    box.atomic(function()
        local t = box.space[space]:get(key)
        if t ~= nil then
            box.space[space]:delete(key)
            if live(t, clock.time()) then out = t.doc end
        end
    end)
    return out
end

-- Refresh-token rotation with reuse detection (OAuth 2.1 / BCP).
--   ok      old token marked used, new token issued in the same family
--   reuse   an already-used token came back: the whole family is revoked
--   invalid unknown or expired
function auth_rotate_refresh(old_key, new_key, new_expires)
    local result = { status = 'invalid' }
    box.atomic(function()
        local t = box.space.refresh:get(old_key)
        local now = clock.time()
        if t == nil or not live(t, now) then return end
        local doc = t.doc
        if doc.used then
            for _, r in box.space.refresh.index.idx:pairs({ t.idx }) do box.space.refresh:delete(r.key) end
            result = { status = 'reuse', family = t.idx, user_id = doc.user_id, sid = doc.sid }
            return
        end
        local used = table.deepcopy(doc)
        used.used = true
        box.space.refresh:replace({ old_key, t.idx, t.expires, used })
        local fresh = table.deepcopy(doc)
        fresh.used = false
        fresh.issued_at = now
        box.space.refresh:insert({ new_key, t.idx, new_expires, fresh })
        result = { status = 'ok', doc = fresh }
    end)
    return result
end

-- Merge fields into a user document atomically.
function auth_update_user(id, patch)
    local out = box.NULL
    box.atomic(function()
        local t = box.space.users:get(id)
        if t == nil then return end
        local doc = table.deepcopy(t.doc)
        for k, v in pairs(patch) do doc[k] = v end
        box.space.users:replace({ t.key, t.idx, t.expires, doc })
        out = doc
    end)
    return out
end

function auth_users_page(offset, limit)
    local out, i = {}, 0
    for _, t in box.space.users:pairs() do
        if i >= offset and #out < limit then table.insert(out, t.doc) end
        i = i + 1
    end
    return { total = box.space.users:len(), items = setmetatable(out, { __serialize = 'array' }) }
end

function auth_count(space)
    return box.space[space]:len()
end

function auth_audit(doc)
    box.space.audit:insert({ box.NULL, clock.time(), doc })
    return true
end

function auth_audit_list(limit, user_id)
    local out = {}
    for _, t in box.space.audit.index.primary:pairs(nil, { iterator = 'REQ' }) do
        if #out >= limit then break end
        if user_id == nil or user_id == box.NULL or t.doc.user_id == user_id then
            local d = table.deepcopy(t.doc)
            d.seq, d.at = t.seq, t.at
            table.insert(out, d)
        end
    end
    return out
end

-- Background sweeper: drop expired codes, challenges, tokens and sessions.
fiber.create(function()
    fiber.name('auth-sweeper')
    while true do
        fiber.sleep(30)
        if not box.info.ro then
            local now = clock.time()
            for _, name in ipairs({ 'codes', 'refresh', 'sessions', 'challenges' }) do
                local keys = {}
                for _, t in box.space[name].index.expires:pairs({ 0.001 }, { iterator = 'GE' }) do
                    if t.expires > now or #keys >= 1000 then break end
                    table.insert(keys, t.key)
                end
                for _, k in ipairs(keys) do box.space[name]:delete(k) end
            end
            -- Keep the audit log bounded (last 50k events).
            local n = box.space.audit:len() - 50000
            for _, t in box.space.audit.index.primary:pairs() do
                if n <= 0 then break end
                box.space.audit:delete(t.seq)
                n = n - 1
            end
        end
    end
end)

log.info('auth storage ready')
