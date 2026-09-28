//! Client for the auth database (auth/tarantool/init.lua). Documents cross the
//! wire as MessagePack maps mirroring JSON.

use anyhow::{anyhow, Context, Result};
use rmpv::Value as Mp;
use serde_json::Value as Json;
use tarantool_rs::{Connection, ExecutorExt};

#[derive(Clone)]
pub struct Db {
    conn: Connection,
}

fn to_mp(v: Json) -> Mp {
    match v {
        Json::Null => Mp::Nil,
        Json::Bool(b) => Mp::Boolean(b),
        Json::Number(n) => n
            .as_u64()
            .map(Mp::from)
            .or_else(|| n.as_i64().map(Mp::from))
            .unwrap_or_else(|| Mp::F64(n.as_f64().unwrap_or_default())),
        Json::String(s) => Mp::from(s),
        Json::Array(a) => Mp::Array(a.into_iter().map(to_mp).collect()),
        Json::Object(o) => Mp::Map(o.into_iter().map(|(k, v)| (Mp::from(k), to_mp(v))).collect()),
    }
}

fn to_json(v: Mp) -> Json {
    match v {
        Mp::Nil => Json::Null,
        Mp::Boolean(b) => Json::Bool(b),
        Mp::Integer(i) => i.as_u64().map(Json::from).or_else(|| i.as_i64().map(Json::from)).unwrap_or(Json::Null),
        Mp::F32(f) => Json::from(f as f64),
        Mp::F64(f) => Json::from(f),
        Mp::String(s) => Json::String(s.into_str().unwrap_or_default()),
        Mp::Array(a) => Json::Array(a.into_iter().map(to_json).collect()),
        Mp::Map(m) => Json::Object(
            m.into_iter()
                .map(|(k, v)| (k.as_str().map(str::to_string).unwrap_or_else(|| k.to_string()), to_json(v)))
                .collect(),
        ),
        _ => Json::Null,
    }
}

fn arr(v: Json) -> Vec<Json> {
    match v {
        Json::Array(a) => a,
        _ => vec![],
    }
}

impl Db {
    pub async fn connect(addr: &str, user: &str, password: &str) -> Result<Self> {
        let conn = Connection::builder()
            .auth(user, Some(password))
            .timeout(std::time::Duration::from_secs(10))
            .reconnect_interval(tarantool_rs::ReconnectInterval::fixed(std::time::Duration::from_secs(1)))
            .build(addr.to_string())
            .await
            .with_context(|| format!("connecting to auth db at {addr}"))?;
        Ok(Db { conn })
    }

    async fn call(&self, f: &str, args: Vec<Mp>) -> Result<Json> {
        let r = self.conn.call(f, args).await.map_err(|e| anyhow!("auth db {f}: {e}"))?;
        let v: Mp = r.decode_first().map_err(|e| anyhow!("auth db {f}: {e}"))?;
        Ok(to_json(v))
    }

    pub async fn get(&self, space: &str, key: &str) -> Result<Option<Json>> {
        Ok(Some(self.call("auth_get", vec![Mp::from(space), Mp::from(key)]).await?).filter(|v| !v.is_null()))
    }

    pub async fn put(&self, space: &str, key: &str, idx: &str, expires: f64, doc: Json) -> Result<()> {
        self.call("auth_put", vec![Mp::from(space), Mp::from(key), Mp::from(idx), Mp::F64(expires), to_mp(doc)])
            .await
            .map(|_| ())
    }

    /// Returns false on a uniqueness conflict.
    pub async fn insert(&self, space: &str, key: &str, idx: &str, expires: f64, doc: Json) -> Result<bool> {
        let v = self
            .call("auth_insert", vec![Mp::from(space), Mp::from(key), Mp::from(idx), Mp::F64(expires), to_mp(doc)])
            .await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    pub async fn delete(&self, space: &str, key: &str) -> Result<()> {
        self.call("auth_delete", vec![Mp::from(space), Mp::from(key)]).await.map(|_| ())
    }

    pub async fn by_idx(&self, space: &str, idx: &str, limit: u32) -> Result<Vec<Json>> {
        Ok(arr(self.call("auth_by_idx", vec![Mp::from(space), Mp::from(idx), Mp::from(limit)]).await?))
    }

    pub async fn user_by_email_index(&self, idx: &str) -> Result<Option<Json>> {
        Ok(Some(self.call("auth_by_idx_one", vec![Mp::from("users"), Mp::from(idx)]).await?).filter(|v| !v.is_null()))
    }

    pub async fn delete_by_idx(&self, space: &str, idx: &str) -> Result<u64> {
        Ok(self.call("auth_delete_by_idx", vec![Mp::from(space), Mp::from(idx)]).await?.as_u64().unwrap_or(0))
    }

    /// Atomic single-use read (authorization codes, challenges, pending requests).
    pub async fn consume(&self, space: &str, key: &str) -> Result<Option<Json>> {
        Ok(Some(self.call("auth_consume", vec![Mp::from(space), Mp::from(key)]).await?).filter(|v| !v.is_null()))
    }

    pub async fn rotate_refresh(&self, old_key: &str, new_key: &str, new_expires: f64) -> Result<Json> {
        self.call("auth_rotate_refresh", vec![Mp::from(old_key), Mp::from(new_key), Mp::F64(new_expires)]).await
    }

    pub async fn update_user(&self, id: &str, patch: Json) -> Result<Option<Json>> {
        Ok(Some(self.call("auth_update_user", vec![Mp::from(id), to_mp(patch)]).await?).filter(|v| !v.is_null()))
    }

    pub async fn users_page(&self, offset: u32, limit: u32) -> Result<(u64, Vec<Json>)> {
        let v = self.call("auth_users_page", vec![Mp::from(offset), Mp::from(limit)]).await?;
        Ok((v["total"].as_u64().unwrap_or(0), arr(v["items"].clone())))
    }

    pub async fn count(&self, space: &str) -> Result<u64> {
        Ok(self.call("auth_count", vec![Mp::from(space)]).await?.as_u64().unwrap_or(0))
    }

    pub async fn audit(&self, doc: Json) -> Result<()> {
        self.call("auth_audit", vec![to_mp(doc)]).await.map(|_| ())
    }

    pub async fn audit_list(&self, limit: u32, user_id: Option<&str>) -> Result<Vec<Json>> {
        Ok(arr(self
            .call("auth_audit_list", vec![Mp::from(limit), user_id.map(Mp::from).unwrap_or(Mp::Nil)])
            .await?))
    }
}
