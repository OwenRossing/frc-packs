//! DynamoDB: one table, every item keyed by `pk` / `sk`, two general indexes (`gsi1`, `gsi2`). See docs/dynamodb.md for
//! the layout and for how the rules are enforced.
//!
//! Everything that changes what a player owns goes through [`Tx`], one DynamoDB transaction, and includes the player's
//! profile written back with its version bumped on condition the version is unchanged ([`Tx::user`]). Two requests for
//! the same player can't both win: the second one's transaction is cancelled and it starts over from a fresh read.

use std::collections::HashMap;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, ConditionCheck, Delete, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, Projection, ProjectionType, Put, ReturnValue, ScalarAttributeType, TransactWriteItem, Update,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub type Item = HashMap<String, AttributeValue>;

#[derive(Clone)]
pub struct Db {
    pub client: Client,
    pub table: String,
}

/// What went wrong talking to DynamoDB.
#[derive(Debug)]
pub enum DbError {
    /// A transaction was cancelled because a condition failed. `label` is the label of the first item whose condition
    /// failed (see [`Tx`]); "" for an unlabeled one. A transaction that lost a race with another one comes back as
    /// `Conflict("race")`.
    Conflict(&'static str),
    Other(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Conflict(l) => write!(f, "condition failed ({l})"),
            DbError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DbError {}

impl DbError {
    /// The request lost a race with another change to the same player (or the same shared counter) and should be
    /// run again from a fresh read.
    pub fn is_race(&self) -> bool {
        matches!(self, DbError::Conflict("ver" | "race" | "print" | "free"))
    }
}

fn other<E: std::fmt::Debug>(e: E) -> DbError {
    DbError::Other(format!("{e:?}"))
}

pub type DbResult<T> = Result<T, DbError>;

// ---------- attribute values ----------

pub fn s(v: impl Into<String>) -> AttributeValue {
    AttributeValue::S(v.into())
}

pub fn n(v: impl std::fmt::Display) -> AttributeValue {
    AttributeValue::N(v.to_string())
}

pub fn get_s(i: &Item, k: &str) -> Option<String> {
    i.get(k).and_then(|v| v.as_s().ok()).cloned()
}

pub fn get_n(i: &Item, k: &str) -> Option<i64> {
    i.get(k).and_then(|v| v.as_n().ok()).and_then(|v| v.parse().ok())
}

pub fn key(pk: impl Into<String>, sk: impl Into<String>) -> Item {
    HashMap::from([("pk".to_string(), s(pk)), ("sk".to_string(), s(sk))])
}

/// A struct as an item, with its key.
pub fn item<T: Serialize>(pk: impl Into<String>, sk: impl Into<String>, v: &T) -> Item {
    let mut it: Item = serde_dynamo::to_item(v).expect("our structs serialize");
    it.retain(|_, v| !matches!(v, AttributeValue::Null(_)));
    it.extend(key(pk, sk));
    it
}

pub fn from_item<T: DeserializeOwned>(it: Item) -> DbResult<T> {
    serde_dynamo::from_item(it).map_err(|e| DbError::Other(format!("bad item: {e}")))
}

/// Zero-padded, so numbers sort as text in sort keys.
pub fn pad(v: i64, width: usize) -> String {
    format!("{v:0width$}")
}

// ---------- reads ----------

impl Db {
    pub async fn get(&self, pk: &str, sk: &str) -> DbResult<Option<Item>> {
        let r = self
            .client
            .get_item()
            .table_name(&self.table)
            .set_key(Some(key(pk, sk)))
            .consistent_read(true)
            .send()
            .await
            .map_err(other)?;
        Ok(r.item)
    }

    pub async fn get_as<T: DeserializeOwned>(&self, pk: &str, sk: &str) -> DbResult<Option<T>> {
        match self.get(pk, sk).await? {
            Some(i) => Ok(Some(from_item(i)?)),
            None => Ok(None),
        }
    }

    /// Every item in `pk` whose sort key starts with `prefix`, in sort key order (consistent read).
    pub async fn query(&self, pk: &str, prefix: &str) -> DbResult<Vec<Item>> {
        self.query_opts(pk, prefix, None, false).await
    }

    /// Like [`Db::query`], newest (highest sort key) first, at most `limit`.
    pub async fn query_desc(&self, pk: &str, prefix: &str, limit: i32) -> DbResult<Vec<Item>> {
        self.query_opts(pk, prefix, Some(limit), true).await
    }

    async fn query_opts(&self, pk: &str, prefix: &str, limit: Option<i32>, desc: bool) -> DbResult<Vec<Item>> {
        let mut out = Vec::new();
        let mut start: Option<Item> = None;
        loop {
            let mut q = self
                .client
                .query()
                .table_name(&self.table)
                .consistent_read(true)
                .scan_index_forward(!desc)
                .set_exclusive_start_key(start.take())
                .expression_attribute_values(":pk", s(pk));
            q = if prefix.is_empty() {
                q.key_condition_expression("pk = :pk")
            } else {
                q.key_condition_expression("pk = :pk AND begins_with(sk, :p)")
                    .expression_attribute_values(":p", s(prefix))
            };
            if let Some(l) = limit {
                q = q.limit(l - out.len() as i32);
            }
            let r = q.send().await.map_err(other)?;
            out.extend(r.items.unwrap_or_default());
            match r.last_evaluated_key {
                Some(k) if limit.is_none_or(|l| (out.len() as i32) < l) => start = Some(k),
                _ => return Ok(out),
            }
        }
    }

    /// Items of a global index (`gsi1` or `gsi2`) under one partition key, by its sort key (eventually consistent:
    /// indexes catch up within a moment). Newest first when `desc`.
    pub async fn index(&self, index: &str, pk: &str, limit: Option<i32>, desc: bool) -> DbResult<Vec<Item>> {
        let mut out = Vec::new();
        let mut start: Option<Item> = None;
        loop {
            let mut q = self
                .client
                .query()
                .table_name(&self.table)
                .index_name(index)
                .scan_index_forward(!desc)
                .set_exclusive_start_key(start.take())
                .key_condition_expression("#k = :pk")
                .expression_attribute_names("#k", format!("{index}pk"))
                .expression_attribute_values(":pk", s(pk));
            if let Some(l) = limit {
                q = q.limit(l - out.len() as i32);
            }
            let r = q.send().await.map_err(other)?;
            out.extend(r.items.unwrap_or_default());
            match r.last_evaluated_key {
                Some(k) if limit.is_none_or(|l| (out.len() as i32) < l) => start = Some(k),
                _ => return Ok(out),
            }
        }
    }

    /// The whole table (admin audit only).
    pub async fn scan_all(&self) -> DbResult<Vec<Item>> {
        let mut out = Vec::new();
        let mut start: Option<Item> = None;
        loop {
            let r = self
                .client
                .scan()
                .table_name(&self.table)
                .consistent_read(true)
                .set_exclusive_start_key(start.take())
                .send()
                .await
                .map_err(other)?;
            out.extend(r.items.unwrap_or_default());
            match r.last_evaluated_key {
                Some(k) => start = Some(k),
                None => return Ok(out),
            }
        }
    }

    /// Many items by key (missing ones are left out), in no particular order.
    pub async fn batch_get(&self, keys: Vec<Item>) -> DbResult<Vec<Item>> {
        let mut out = Vec::new();
        for chunk in keys.chunks(100) {
            let mut pending = chunk.to_vec();
            while !pending.is_empty() {
                let ka = aws_sdk_dynamodb::types::KeysAndAttributes::builder()
                    .set_keys(Some(pending.clone()))
                    .consistent_read(true)
                    .build()
                    .map_err(other)?;
                let r = self.client.batch_get_item().request_items(&self.table, ka).send().await.map_err(other)?;
                if let Some(mut res) = r.responses {
                    out.extend(res.remove(&self.table).unwrap_or_default());
                }
                pending =
                    r.unprocessed_keys.and_then(|mut u| u.remove(&self.table)).map(|k| k.keys).unwrap_or_default();
            }
        }
        Ok(out)
    }

    // ---------- single writes (for things that aren't anyone's holdings) ----------

    pub async fn put(&self, it: Item) -> DbResult<()> {
        self.client.put_item().table_name(&self.table).set_item(Some(it)).send().await.map_err(other)?;
        Ok(())
    }

    pub async fn delete(&self, pk: &str, sk: &str) -> DbResult<()> {
        self.client.delete_item().table_name(&self.table).set_key(Some(key(pk, sk))).send().await.map_err(other)?;
        Ok(())
    }

    /// Adds `by` to a counter attribute and returns the new value. Used for ids (cards, openings, trades): a
    /// transaction that later fails leaves a gap in the ids, never a duplicate.
    pub async fn bump(&self, pk: &str, sk: &str, attr: &str, by: i64) -> DbResult<i64> {
        let r = self
            .client
            .update_item()
            .table_name(&self.table)
            .set_key(Some(key(pk, sk)))
            .update_expression("ADD #a :n")
            .expression_attribute_names("#a", attr)
            .expression_attribute_values(":n", n(by))
            .return_values(ReturnValue::UpdatedNew)
            .send()
            .await
            .map_err(other)?;
        r.attributes.as_ref().and_then(|a| get_n(a, attr)).ok_or_else(|| DbError::Other("counter missing".into()))
    }

    /// `n` new ids of a kind (card, open, trade, report, special): returns the first.
    pub async fn ids(&self, kind: &str, count: i64) -> DbResult<i64> {
        Ok(self.bump("CTR", "CTR", kind, count).await? - count + 1)
    }

    /// Makes the table if it doesn't exist (local development and tests; on AWS the stack makes it).
    pub async fn ensure_table(&self) -> DbResult<()> {
        let exists = self.client.describe_table().table_name(&self.table).send().await.is_ok();
        if exists {
            return Ok(());
        }
        let attr = |name: &str| {
            AttributeDefinition::builder().attribute_name(name).attribute_type(ScalarAttributeType::S).build().unwrap()
        };
        let ks = |name: &str, t: KeyType| KeySchemaElement::builder().attribute_name(name).key_type(t).build().unwrap();
        let gsi = |name: &str| {
            GlobalSecondaryIndex::builder()
                .index_name(name)
                .key_schema(ks(&format!("{name}pk"), KeyType::Hash))
                .key_schema(ks(&format!("{name}sk"), KeyType::Range))
                .projection(Projection::builder().projection_type(ProjectionType::All).build())
                .build()
                .unwrap()
        };
        let r = self
            .client
            .create_table()
            .table_name(&self.table)
            .billing_mode(BillingMode::PayPerRequest)
            .set_attribute_definitions(Some(
                ["pk", "sk", "gsi1pk", "gsi1sk", "gsi2pk", "gsi2sk"].iter().map(|a| attr(a)).collect(),
            ))
            .key_schema(ks("pk", KeyType::Hash))
            .key_schema(ks("sk", KeyType::Range))
            .global_secondary_indexes(gsi("gsi1"))
            .global_secondary_indexes(gsi("gsi2"))
            .send()
            .await;
        match r {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some("ResourceInUseException") => Ok(()),
            Err(e) => Err(other(e)),
        }
    }
}

// ---------- transactions ----------

/// One DynamoDB transaction: up to 100 writes and checks that all happen or none do. Each write can carry a label;
/// when its condition fails, the commit returns `DbError::Conflict(label)` so the caller can tell which rule stopped it.
#[derive(Default)]
pub struct Tx {
    items: Vec<TransactWriteItem>,
    labels: Vec<&'static str>,
}

/// A condition: the expression plus its placeholders (`#name` and `:value`).
#[derive(Default, Clone)]
pub struct Cond {
    pub expr: String,
    pub names: HashMap<String, String>,
    pub values: Item,
}

impl Cond {
    pub fn new(expr: impl Into<String>) -> Cond {
        Cond { expr: expr.into(), ..Default::default() }
    }

    pub fn name(mut self, k: &str, v: &str) -> Cond {
        self.names.insert(k.into(), v.into());
        self
    }

    pub fn val(mut self, k: &str, v: AttributeValue) -> Cond {
        self.values.insert(k.into(), v);
        self
    }

    /// The item exists.
    pub fn exists() -> Cond {
        Cond::new("attribute_exists(pk)")
    }

    /// The item doesn't exist yet.
    pub fn absent() -> Cond {
        Cond::new("attribute_not_exists(pk)")
    }
}

fn opt<T: Clone>(m: &HashMap<String, T>) -> Option<HashMap<String, T>> {
    (!m.is_empty()).then(|| m.clone())
}

impl Tx {
    pub fn new() -> Tx {
        Tx::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn push(&mut self, label: &'static str, it: TransactWriteItem) {
        self.items.push(it);
        self.labels.push(label);
    }

    /// Writes an item, replacing any old one.
    pub fn put(&mut self, it: Item) {
        self.put_if("", it, None);
    }

    /// Writes an item on a condition.
    pub fn put_if(&mut self, label: &'static str, it: Item, cond: Option<Cond>) {
        let mut p = Put::builder().table_name("").set_item(Some(it));
        if let Some(c) = cond {
            p = p
                .condition_expression(c.expr.clone())
                .set_expression_attribute_names(opt(&c.names))
                .set_expression_attribute_values(opt(&c.values));
        }
        self.push(label, TransactWriteItem::builder().put(p.build().unwrap()).build());
    }

    /// Changes an item with an update expression (`SET`, `ADD`, `REMOVE`), on an optional condition.
    pub fn update(&mut self, label: &'static str, pk: &str, sk: &str, expr: Cond, cond: Option<&str>) {
        let mut u = Update::builder()
            .table_name("")
            .set_key(Some(key(pk, sk)))
            .update_expression(expr.expr.clone())
            .set_expression_attribute_names(opt(&expr.names))
            .set_expression_attribute_values(opt(&expr.values));
        if let Some(c) = cond {
            u = u.condition_expression(c);
        }
        self.push(label, TransactWriteItem::builder().update(u.build().unwrap()).build());
    }

    /// Deletes an item, on an optional condition.
    pub fn delete_if(&mut self, label: &'static str, pk: &str, sk: &str, cond: Option<Cond>) {
        let mut d = Delete::builder().table_name("").set_key(Some(key(pk, sk)));
        if let Some(c) = cond {
            d = d
                .condition_expression(c.expr.clone())
                .set_expression_attribute_names(opt(&c.names))
                .set_expression_attribute_values(opt(&c.values));
        }
        self.push(label, TransactWriteItem::builder().delete(d.build().unwrap()).build());
    }

    /// Checks a condition on an item without changing it.
    pub fn check(&mut self, label: &'static str, pk: &str, sk: &str, cond: Cond) {
        let c = ConditionCheck::builder()
            .table_name("")
            .set_key(Some(key(pk, sk)))
            .condition_expression(cond.expr.clone())
            .set_expression_attribute_names(opt(&cond.names))
            .set_expression_attribute_values(opt(&cond.values));
        self.push(label, TransactWriteItem::builder().condition_check(c.build().unwrap()).build());
    }

    /// Runs the transaction.
    pub async fn commit(self, db: &Db) -> DbResult<()> {
        if self.items.is_empty() {
            return Ok(());
        }
        if self.items.len() > 100 {
            return Err(DbError::Other(format!("transaction too big: {} items", self.items.len())));
        }
        // The table name is filled in here so callers don't have to pass it around.
        let items: Vec<TransactWriteItem> = self.items.into_iter().map(|it| with_table(it, &db.table)).collect();
        match db.client.transact_write_items().set_transact_items(Some(items)).send().await {
            Ok(_) => Ok(()),
            Err(SdkError::ServiceError(e)) => {
                let e = e.into_err();
                if let aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError::TransactionCanceledException(c) = &e {
                    let reasons = c.cancellation_reasons();
                    if let Some(i) = reasons.iter().position(|r| r.code() == Some("ConditionalCheckFailed")) {
                        return Err(DbError::Conflict(self.labels.get(i).copied().unwrap_or("")));
                    }
                    if reasons.iter().any(|r| matches!(r.code(), Some("TransactionConflict"))) {
                        return Err(DbError::Conflict("race"));
                    }
                }
                if e.code() == Some("TransactionConflictException") {
                    return Err(DbError::Conflict("race"));
                }
                Err(DbError::Other(format!("{e:?}")))
            }
            Err(e) => Err(other(e)),
        }
    }
}

fn with_table(it: TransactWriteItem, table: &str) -> TransactWriteItem {
    let mut b = TransactWriteItem::builder();
    if let Some(p) = it.put {
        let mut p2 = Put::builder()
            .table_name(table)
            .set_item(Some(p.item))
            .set_expression_attribute_names(p.expression_attribute_names)
            .set_expression_attribute_values(p.expression_attribute_values);
        if let Some(c) = p.condition_expression {
            p2 = p2.condition_expression(c);
        }
        b = b.put(p2.build().unwrap());
    }
    if let Some(u) = it.update {
        let mut u2 = Update::builder()
            .table_name(table)
            .set_key(Some(u.key))
            .update_expression(u.update_expression)
            .set_expression_attribute_names(u.expression_attribute_names)
            .set_expression_attribute_values(u.expression_attribute_values);
        if let Some(c) = u.condition_expression {
            u2 = u2.condition_expression(c);
        }
        b = b.update(u2.build().unwrap());
    }
    if let Some(d) = it.delete {
        let mut d2 = Delete::builder()
            .table_name(table)
            .set_key(Some(d.key))
            .set_expression_attribute_names(d.expression_attribute_names)
            .set_expression_attribute_values(d.expression_attribute_values);
        if let Some(c) = d.condition_expression {
            d2 = d2.condition_expression(c);
        }
        b = b.delete(d2.build().unwrap());
    }
    if let Some(c) = it.condition_check {
        let c2 = ConditionCheck::builder()
            .table_name(table)
            .set_key(Some(c.key))
            .condition_expression(c.condition_expression)
            .set_expression_attribute_names(c.expression_attribute_names)
            .set_expression_attribute_values(c.expression_attribute_values);
        b = b.condition_check(c2.build().unwrap());
    }
    b.build()
}

/// Connects to DynamoDB: AWS credentials and region from the environment, or a local DynamoDB when
/// `DYNAMODB_ENDPOINT` is set (e.g. http://localhost:8765).
pub async fn connect(table: &str) -> Db {
    let mut cfg = aws_config::defaults(aws_config::BehaviorVersion::latest());
    if let Ok(endpoint) = std::env::var("DYNAMODB_ENDPOINT") {
        cfg = cfg
            .endpoint_url(endpoint)
            .region(aws_config::Region::new(std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".into())))
            .credentials_provider(aws_sdk_dynamodb::config::Credentials::new("local", "local", None, None, "local"));
    }
    let cfg = cfg.load().await;
    Db { client: Client::new(&cfg), table: table.to_string() }
}
