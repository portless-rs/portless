use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub hostname: String,
    pub port: u16,
    pub pid: u32,
}
