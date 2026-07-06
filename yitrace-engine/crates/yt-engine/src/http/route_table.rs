/// 远端 shard 动态路由表的版本模型。
///
/// v1 兼容扁平 `shards:[{id,addr,role,...}]`；v2 增加 logical shard
/// + replicas：`shards:[{shardId,replicas:[{replicaId,addr,role,...}]}]`。
/// 当前 gateway 写路径仍要求每个 logical shard 恰好一个 writable replica，
/// 这样手动 promote 只需要 reload route table，不会把同一 shard 写成双主。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteShardRouteTable {
    version: u64,
    shards: Vec<RemoteShardRoute>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteShardRoute {
    shard_id: String,
    replica_id: String,
    addr: String,
    role: RemoteShardRouteRole,
    readable: bool,
    writable: bool,
    weight: u32,
    priority: u32,
    max_lag_lsn: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteShardRouteRole {
    Leader,
    Follower,
    Candidate,
    Unknown,
}

impl RemoteShardRouteRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Follower => "follower",
            Self::Candidate => "candidate",
            Self::Unknown => "unknown",
        }
    }
}

impl RemoteShardRouteTable {
    pub fn parse_json(body: &str) -> Result<Self, String> {
        let root = crate::wire::parse(body)?;
        let version = json_field_alias(
            &root,
            &["version", "routeTableVersion", "route_table_version"],
        )
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "route table requires version".to_string())?;
        let shard_items = json_field_alias(&root, &["shards", "routes"])
            .ok_or_else(|| "route table requires shards".to_string())?
            .as_array();
        if shard_items.is_empty() {
            return Err("route table requires at least one shard".to_string());
        }

        let mut seen_logical = std::collections::BTreeSet::new();
        let mut seen_replicas = std::collections::BTreeSet::new();
        let mut shards = Vec::new();
        for (idx, item) in shard_items.iter().enumerate() {
            let shard_id = route_table_string_alias(
                item,
                &["id", "shardId", "shard_id", "logicalShardId", "logical_shard_id"],
            )
                .unwrap_or_else(|| format!("shard-{idx}"));
            if shard_id.trim().is_empty() {
                return Err(format!("route table shard {idx} has empty id"));
            }
            if let Some(replicas) = json_field_alias(item, &["replicas"]).map(|v| v.as_array()) {
                if replicas.is_empty() {
                    return Err(format!(
                        "route table logical shard {shard_id} requires at least one replica"
                    ));
                }
                if !seen_logical.insert(shard_id.clone()) {
                    return Err(format!(
                        "route table contains duplicate shard id {shard_id}"
                    ));
                }
                let start_len = shards.len();
                for (replica_idx, replica) in replicas.iter().enumerate() {
                    let default_replica_id = format!("{shard_id}-replica-{replica_idx}");
                    let route =
                        parse_route_table_route(replica, &shard_id, &default_replica_id)?;
                    let replica_key = format!("{}/{}", route.shard_id, route.replica_id);
                    if !seen_replicas.insert(replica_key) {
                        return Err(format!(
                            "route table shard {shard_id} has duplicate replica id {}",
                            route.replica_id
                        ));
                    }
                    shards.push(route);
                }
                let writable = shards[start_len..]
                    .iter()
                    .filter(|route| route.writable)
                    .count();
                if writable != 1 {
                    return Err(format!(
                        "route table logical shard {shard_id} requires exactly one writable replica, got {writable}"
                    ));
                }
            } else {
                // v1 flat route table: every item is already a route. This path intentionally
                // allows follower rows with `writable:false` for backward compatibility.
                if !seen_logical.insert(shard_id.clone()) {
                    return Err(format!(
                        "route table contains duplicate shard id {shard_id}"
                    ));
                }
                let route = parse_route_table_route(item, &shard_id, &shard_id)?;
                let replica_key = format!("{}/{}", route.shard_id, route.replica_id);
                if !seen_replicas.insert(replica_key) {
                    return Err(format!(
                        "route table contains duplicate replica id {}",
                        route.replica_id
                    ));
                }
                shards.push(route);
            }
        }

        Ok(Self { version, shards })
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn shards(&self) -> &[RemoteShardRoute] {
        &self.shards
    }

    pub fn write_addrs(&self) -> Vec<String> {
        self.write_routes()
            .into_iter()
            .map(|shard| shard.addr.clone())
            .collect()
    }

    pub fn write_routes(&self) -> Vec<&RemoteShardRoute> {
        self.shards
            .iter()
            .filter(|shard| shard.writable)
            .collect()
    }

    pub fn read_addrs(&self) -> Vec<String> {
        self.shards
            .iter()
            .filter(|shard| shard.readable)
            .map(|shard| shard.addr.clone())
            .collect()
    }

    pub fn find(&self, shard_id: &str) -> Option<&RemoteShardRoute> {
        self.shards
            .iter()
            .find(|shard| shard.shard_id == shard_id || shard.replica_id == shard_id)
    }

    pub fn replicas_for_shard_id(&self, shard_id: &str) -> Vec<&RemoteShardRoute> {
        self.shards
            .iter()
            .filter(|shard| shard.shard_id == shard_id)
            .collect()
    }

    pub fn fingerprint(&self) -> String {
        let mut entries: Vec<String> = self
            .shards
            .iter()
            .map(|shard| {
                format!(
                    "{}/{}@{}:{:?}:r{}:w{}:{}:{}:{}",
                    shard.shard_id,
                    shard.replica_id,
                    shard.addr,
                    shard.role,
                    shard.readable as u8,
                    shard.writable as u8,
                    shard.weight,
                    shard.priority,
                    shard
                        .max_lag_lsn
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string())
                )
            })
            .collect();
        entries.sort();
        format!("v{}|{}", self.version, entries.join("|"))
    }
}

impl RemoteShardRoute {
    pub fn id(&self) -> &str {
        &self.shard_id
    }

    pub fn replica_id(&self) -> &str {
        &self.replica_id
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn role(&self) -> RemoteShardRouteRole {
        self.role
    }

    pub fn readable(&self) -> bool {
        self.readable
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    pub fn weight(&self) -> u32 {
        self.weight
    }

    pub fn priority(&self) -> u32 {
        self.priority
    }

    pub fn max_lag_lsn(&self) -> Option<u64> {
        self.max_lag_lsn
    }
}

fn parse_route_table_route(
    obj: &crate::wire::Json,
    shard_id: &str,
    default_replica_id: &str,
) -> Result<RemoteShardRoute, String> {
    let replica_id = route_table_string_alias(obj, &["replicaId", "replica_id", "id"])
        .unwrap_or_else(|| default_replica_id.to_string());
    if replica_id.trim().is_empty() {
        return Err(format!(
            "route table shard {shard_id} has empty replica id"
        ));
    }
    let raw_addr = route_table_string_alias(obj, &["addr", "address", "url", "endpoint"])
        .ok_or_else(|| format!("route table shard {shard_id} replica {replica_id} requires addr"))?;
    let addr = normalize_remote_addr(&raw_addr);
    if addr.is_empty() {
        return Err(format!(
            "route table shard {shard_id} replica {replica_id} has empty addr"
        ));
    }

    let role = route_table_role_alias(obj, &["role"]).unwrap_or(RemoteShardRouteRole::Leader);
    let readable = route_table_bool_alias(obj, &["read", "readable"]).unwrap_or(true);
    let writable = route_table_bool_alias(obj, &["write", "writable"])
        .unwrap_or(matches!(role, RemoteShardRouteRole::Leader));
    let weight = route_table_u32_alias(obj, &["weight"], 1)?;
    let priority = route_table_u32_alias(obj, &["priority"], 0)?;
    let max_lag_lsn = json_field_alias(obj, &["maxLagLsn", "max_lag_lsn"])
        .and_then(crate::wire::Json::as_u64);

    Ok(RemoteShardRoute {
        shard_id: shard_id.to_string(),
        replica_id,
        addr,
        role,
        readable,
        writable,
        weight,
        priority,
        max_lag_lsn,
    })
}

fn route_table_u32_alias(
    obj: &crate::wire::Json,
    names: &[&str],
    default_value: u32,
) -> Result<u32, String> {
    let Some(value) = json_field_alias(obj, names).and_then(crate::wire::Json::as_u64) else {
        return Ok(default_value);
    };
    if value == 0 && names.contains(&"weight") {
        return Err("route table route has invalid weight 0".to_string());
    }
    if value > u32::MAX as u64 {
        return Err(format!("route table route value {value} is too large"));
    }
    Ok(value as u32)
}

fn route_table_string_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<String> {
    json_field_alias(obj, names)
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
}

fn route_table_bool_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<bool> {
    match json_field_alias(obj, names)? {
        crate::wire::Json::Bool(value) => Some(*value),
        crate::wire::Json::Num(value) | crate::wire::Json::Str(value) => match value.as_str() {
            "1" | "true" | "TRUE" | "True" => Some(true),
            "0" | "false" | "FALSE" | "False" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn route_table_role_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<RemoteShardRouteRole> {
    let role = route_table_string_alias(obj, names)?;
    Some(match role.as_str() {
        "leader" | "primary" | "writer" => RemoteShardRouteRole::Leader,
        "follower" | "replica" | "reader" => RemoteShardRouteRole::Follower,
        "candidate" => RemoteShardRouteRole::Candidate,
        _ => RemoteShardRouteRole::Unknown,
    })
}

#[cfg(test)]
mod route_table_tests {
    use super::*;

    #[test]
    fn route_table_parses_version_roles_and_normalized_addrs() {
        let table = RemoteShardRouteTable::parse_json(
            r#"{
              "routeTableVersion":42,
              "shards":[
                {"shardId":"s0","url":"http://127.0.0.1:9100","role":"leader","weight":2},
                {"id":"s0-r1","addr":"127.0.0.1:9101/path","role":"follower","readable":true,"writable":false}
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(table.version(), 42);
        assert_eq!(table.shards().len(), 2);
        assert_eq!(table.write_addrs(), vec!["127.0.0.1:9100".to_string()]);
        assert_eq!(
            table.read_addrs(),
            vec!["127.0.0.1:9100".to_string(), "127.0.0.1:9101".to_string()]
        );
        let follower = table.find("s0-r1").unwrap();
        assert_eq!(follower.id(), "s0-r1");
        assert_eq!(follower.replica_id(), "s0-r1");
        assert_eq!(follower.role(), RemoteShardRouteRole::Follower);
        assert!(follower.readable());
        assert!(!follower.writable());
    }

    #[test]
    fn route_table_rejects_duplicate_ids_and_bad_weights() {
        let duplicate = RemoteShardRouteTable::parse_json(
            r#"{"version":1,"shards":[{"id":"s0","addr":"127.0.0.1:1"},{"id":"s0","addr":"127.0.0.1:2"}]}"#,
        )
        .unwrap_err();
        assert!(duplicate.contains("duplicate shard id"), "{duplicate}");

        let bad_weight = RemoteShardRouteTable::parse_json(
            r#"{"version":1,"shards":[{"id":"s0","addr":"127.0.0.1:1","weight":0}]}"#,
        )
        .unwrap_err();
        assert!(bad_weight.contains("invalid weight"), "{bad_weight}");
    }

    #[test]
    fn route_table_fingerprint_is_versioned_and_order_stable() {
        let a = RemoteShardRouteTable::parse_json(
            r#"{"version":7,"shards":[{"id":"b","addr":"127.0.0.1:2"},{"id":"a","addr":"127.0.0.1:1"}]}"#,
        )
        .unwrap();
        let b = RemoteShardRouteTable::parse_json(
            r#"{"version":7,"shards":[{"id":"a","addr":"127.0.0.1:1"},{"id":"b","addr":"127.0.0.1:2"}]}"#,
        )
        .unwrap();
        let c = RemoteShardRouteTable::parse_json(
            r#"{"version":8,"shards":[{"id":"a","addr":"127.0.0.1:1"},{"id":"b","addr":"127.0.0.1:2"}]}"#,
        )
        .unwrap();

        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn route_table_v2_groups_replicas_and_selects_one_writable_leader() {
        let table = RemoteShardRouteTable::parse_json(
            r#"{
              "version":12,
              "shards":[
                {
                  "shardId":"logical-a",
                  "replicas":[
                    {"replicaId":"a-leader","addr":"http://127.0.0.1:9200","role":"leader","readable":true,"writable":true,"priority":10},
                    {"replicaId":"a-follower","addr":"127.0.0.1:9201","role":"follower","readable":true,"writable":false,"maxLagLsn":7}
                  ]
                },
                {
                  "shardId":"logical-b",
                  "replicas":[
                    {"replicaId":"b-leader","addr":"127.0.0.1:9202","role":"leader","readable":true,"writable":true}
                  ]
                }
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            table.write_addrs(),
            vec![
                "127.0.0.1:9200".to_string(),
                "127.0.0.1:9202".to_string()
            ]
        );
        let replicas = table.replicas_for_shard_id("logical-a");
        assert_eq!(replicas.len(), 2);
        assert_eq!(replicas[0].id(), "logical-a");
        assert_eq!(replicas[0].replica_id(), "a-leader");
        assert_eq!(replicas[0].priority(), 10);
        assert_eq!(replicas[1].max_lag_lsn(), Some(7));
        assert!(table.fingerprint().contains("logical-a/a-leader@"));
    }

    #[test]
    fn route_table_v2_rejects_dual_writers_in_one_logical_shard() {
        let err = RemoteShardRouteTable::parse_json(
            r#"{
              "version":12,
              "shards":[
                {"shardId":"logical-a","replicas":[
                  {"replicaId":"a","addr":"127.0.0.1:1","role":"leader","writable":true},
                  {"replicaId":"b","addr":"127.0.0.1:2","role":"leader","writable":true}
                ]}
              ]
            }"#,
        )
        .unwrap_err();
        assert!(err.contains("exactly one writable replica"), "{err}");
    }
}
