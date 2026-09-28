//! Tarantool access. All reads/writes go through Lua stored procedures
//! defined in `tarantool/init.lua`; documents cross the wire as MessagePack
//! maps that mirror the JSON shape of the core types.

use anyhow::{anyhow, Context, Result};
use chaindeal_core::{Account, Block, Deal, TxRecord};
use rmpv::Value as Mp;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as Json;
use tarantool_rs::{Connection, ExecutorExt};

/// Connection to the Tarantool master, plus an optional read connection
/// (a replica, or a Service balancing across replicas) for read-only queries.
#[derive(Clone)]
pub struct Db {
    conn: Connection,
    ro: Option<Connection>,
}

pub fn json_to_mp(v: Json) -> Mp {
    match v {
        Json::Null => Mp::Nil,
        Json::Bool(b) => Mp::Boolean(b),
        Json::Number(n) => {
            if let Some(u) = n.as_u64() {
                Mp::from(u)
            } else if let Some(i) = n.as_i64() {
                Mp::from(i)
            } else {
                Mp::F64(n.as_f64().unwrap_or_default())
            }
        }
        Json::String(s) => Mp::from(s),
        Json::Array(a) => Mp::Array(a.into_iter().map(json_to_mp).collect()),
        Json::Object(o) => Mp::Map(o.into_iter().map(|(k, v)| (Mp::from(k), json_to_mp(v))).collect()),
    }
}

pub fn mp_to_json(v: Mp) -> Json {
    match v {
        Mp::Nil => Json::Null,
        Mp::Boolean(b) => Json::Bool(b),
        Mp::Integer(i) => i
            .as_u64()
            .map(Json::from)
            .or_else(|| i.as_i64().map(Json::from))
            .unwrap_or(Json::Null),
        Mp::F32(f) => Json::from(f as f64),
        Mp::F64(f) => Json::from(f),
        Mp::String(s) => Json::String(s.into_str().unwrap_or_default()),
        Mp::Binary(b) => Json::String(hex_encode(&b)),
        Mp::Array(a) => Json::Array(a.into_iter().map(mp_to_json).collect()),
        Mp::Map(m) => Json::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        Mp::String(s) => s.into_str().unwrap_or_default(),
                        other => other.to_string(),
                    };
                    (key, mp_to_json(v))
                })
                .collect(),
        ),
        Mp::Ext(..) => Json::Null,
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn to_mp<T: Serialize>(v: &T) -> Mp {
    json_to_mp(serde_json::to_value(v).expect("serializable"))
}

fn from_json<T: DeserializeOwned>(v: Json) -> Result<T> {
    serde_json::from_value(v).context("decoding document from tarantool")
}

impl Db {
    pub async fn connect(addr: &str, user: &str, password: &str) -> Result<Self> {
        Ok(Self { conn: Self::open(addr, user, password).await?, ro: None })
    }

    /// Adds a read connection used by [`Db::reader`].
    pub async fn with_reader(mut self, addr: &str, user: &str, password: &str) -> Result<Self> {
        self.ro = Some(Self::open(addr, user, password).await?);
        Ok(self)
    }

    /// A handle that sends queries to the read replica(s) when configured.
    /// Only for reads that tolerate replication lag (lists, stats, verification).
    pub fn reader(&self) -> Db {
        Db { conn: self.ro.clone().unwrap_or_else(|| self.conn.clone()), ro: None }
    }

    pub fn has_reader(&self) -> bool {
        self.ro.is_some()
    }

    async fn open(addr: &str, user: &str, password: &str) -> Result<Connection> {
        let conn = Connection::builder()
            .auth(user, Some(password))
            .timeout(std::time::Duration::from_secs(10))
            .reconnect_interval(tarantool_rs::ReconnectInterval::fixed(std::time::Duration::from_secs(1)))
            .build(addr.to_string())
            .await
            .with_context(|| format!("connecting to tarantool at {addr}"))?;
        Ok(conn)
    }

    /// Calls a stored procedure and returns its first result as JSON.
    pub async fn call(&self, func: &str, args: Vec<Mp>) -> Result<Json> {
        let resp = self
            .conn
            .call(func, args)
            .await
            .map_err(|e| anyhow!("tarantool {func}: {e}"))?;
        let v: Mp = resp.decode_first().map_err(|e| anyhow!("tarantool {func}: {e}"))?;
        Ok(mp_to_json(v))
    }

    /// Empty Lua tables may arrive as nil or as an empty array.
    fn list<T: DeserializeOwned>(v: Json) -> Result<Vec<T>> {
        match v {
            Json::Array(items) => items.into_iter().map(from_json).collect(),
            Json::Null => Ok(vec![]),
            other => Err(anyhow!("expected array, got {other}")),
        }
    }

    pub async fn tip(&self) -> Result<Option<Block>> {
        match self.call("cd_tip", vec![]).await? {
            Json::Null => Ok(None),
            v => Ok(Some(from_json(v)?)),
        }
    }

    pub async fn get_accounts(&self, addrs: &[String]) -> Result<Vec<Account>> {
        Self::list(self.call("cd_get_accounts", vec![to_mp(&addrs)]).await?)
    }

    pub async fn get_deals(&self, ids: &[String]) -> Result<Vec<Deal>> {
        Self::list(self.call("cd_get_deals", vec![to_mp(&ids)]).await?)
    }

    pub async fn mempool_add(&self, tx: &TxRecord) -> Result<()> {
        self.call("cd_mempool_add", vec![to_mp(tx)]).await.map(|_| ())
    }

    pub async fn mempool_take(&self, limit: u32) -> Result<Vec<TxRecord>> {
        Self::list(self.call("cd_mempool_take", vec![Mp::from(limit)]).await?)
    }

    pub async fn commit_block(
        &self,
        block: Option<&Block>,
        txs: &[TxRecord],
        accounts: &[&Account],
        deals: &[&Deal],
    ) -> Result<()> {
        let block = block.map(to_mp).unwrap_or(Mp::Nil);
        self.call("cd_commit_block", vec![block, to_mp(&txs), to_mp(&accounts), to_mp(&deals)])
            .await
            .map(|_| ())
    }

    /// Newest-first deals, optionally filtered by type and status (`"open"` = non-terminal).
    pub async fn list_deals(&self, limit: u32, offset: u32, dtype: Option<&str>, status: Option<&str>) -> Result<Vec<Deal>> {
        let opt = |v: Option<&str>| v.map(Mp::from).unwrap_or(Mp::Nil);
        Self::list(
            self.call("cd_list_deals", vec![Mp::from(limit), Mp::from(offset), opt(dtype), opt(status)])
                .await?,
        )
    }

    // ---- cluster coordination -------------------------------------------

    pub async fn lease(&self, name: &str, holder: &str, ttl_secs: f64) -> Result<bool> {
        let v = self.call("cd_lease", vec![Mp::from(name), Mp::from(holder), Mp::F64(ttl_secs)]).await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn lease_holder(&self, name: &str) -> Result<Option<String>> {
        Ok(self.call("cd_lease_holder", vec![Mp::from(name)]).await?.as_str().map(str::to_string))
    }

    pub async fn publish(&self, items: Vec<String>) -> Result<()> {
        self.call("cd_publish", vec![Mp::Array(items.into_iter().map(Mp::from).collect())]).await.map(|_| ())
    }

    pub async fn events_head(&self) -> Result<u64> {
        Ok(self.call("cd_events_head", vec![]).await?.as_u64().unwrap_or(0))
    }

    /// `(seq, json)` pairs after `seq`.
    pub async fn events_since(&self, seq: u64, limit: u32) -> Result<Vec<(u64, String)>> {
        let v = self.call("cd_events_since", vec![Mp::from(seq), Mp::from(limit)]).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get(0)?.as_u64()?, e.get(1)?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn metric_add(&self, rows: &[(u64, [u32; 4])]) -> Result<()> {
        let rows: Vec<Mp> = rows
            .iter()
            .map(|(s, c)| Mp::Array(vec![Mp::from(*s), Mp::from(c[0]), Mp::from(c[1]), Mp::from(c[2]), Mp::from(c[3])]))
            .collect();
        self.call("cd_metric_add", vec![Mp::Array(rows)]).await.map(|_| ())
    }

    pub async fn metrics(&self, from_sec: u64) -> Result<Vec<(u64, [u32; 4])>> {
        let v = self.call("cd_metrics", vec![Mp::from(from_sec)]).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let n = |i: usize| r.get(i).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                        Some((r.get(0)?.as_u64()?, [n(1), n(2), n(3), n(4)]))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn prune(&self, max_age_secs: u64) -> Result<()> {
        self.call("cd_prune", vec![Mp::from(max_age_secs)]).await.map(|_| ())
    }

    pub async fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.call("cd_kv_get", vec![Mp::from(key)]).await?.as_str().map(str::to_string))
    }

    pub async fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.call("cd_kv_set", vec![Mp::from(key), Mp::from(value)]).await.map(|_| ())
    }

    pub async fn heartbeat(&self, id: &str, info: &str) -> Result<()> {
        self.call("cd_heartbeat", vec![Mp::from(id), Mp::from(info)]).await.map(|_| ())
    }

    /// Blocks with their transactions, starting at `from` (inclusive).
    pub async fn blocks_range(&self, from: u64, limit: u32) -> Result<Vec<(Block, Vec<TxRecord>)>> {
        #[derive(serde::Deserialize)]
        struct Row {
            block: Block,
            txs: Json,
        }
        let rows: Vec<Row> = Self::list(self.call("cd_blocks_range", vec![Mp::from(from), Mp::from(limit)]).await?)?;
        rows.into_iter()
            .map(|r| Ok((r.block, Self::list(r.txs)?)))
            .collect()
    }
}
