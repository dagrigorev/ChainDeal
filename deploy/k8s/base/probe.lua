-- Kubernetes probe: healthy once this instance is running (replicas: joined and following).
local user = os.getenv('PROBE_USER') or 'chaindeal'
local pass = os.getenv('PROBE_PASSWORD') or os.getenv('CHAINDEAL_DB_PASSWORD') or ''
local ok, c = pcall(require('net.box').connect, user .. ':' .. pass .. '@127.0.0.1:3301', { connect_timeout = 2 })
if not ok or not c:is_connected() then os.exit(1) end
local status = c:eval('return box.info.status', {}, { timeout = 2 })
os.exit(status == 'running' and 0 or 1)
