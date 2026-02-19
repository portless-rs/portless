use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Route {
    pub hostname: String,
    pub port: u16,
    pub pid: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_creation() {
        let route = Route {
            hostname: "test.localhost".to_string(),
            port: 4000,
            pid: 12345,
        };

        assert_eq!(route.hostname, "test.localhost");
        assert_eq!(route.port, 4000);
        assert_eq!(route.pid, 12345);
    }

    #[test]
    fn test_route_serialization() {
        let route = Route {
            hostname: "app.localhost".to_string(),
            port: 4500,
            pid: 99999,
        };

        let json = serde_json::to_string(&route).unwrap();
        assert!(json.contains("app.localhost"));
        assert!(json.contains("4500"));
        assert!(json.contains("99999"));
    }

    #[test]
    fn test_route_deserialization() {
        let json = r#"{"hostname":"test.localhost","port":4200,"pid":54321}"#;
        let route: Route = serde_json::from_str(json).unwrap();

        assert_eq!(route.hostname, "test.localhost");
        assert_eq!(route.port, 4200);
        assert_eq!(route.pid, 54321);
    }

    #[test]
    fn test_route_clone() {
        let route1 = Route {
            hostname: "clone.localhost".to_string(),
            port: 4100,
            pid: 11111,
        };

        let route2 = route1.clone();
        assert_eq!(route1, route2);
    }

    #[test]
    fn test_route_equality() {
        let route1 = Route {
            hostname: "test.localhost".to_string(),
            port: 4000,
            pid: 12345,
        };

        let route2 = Route {
            hostname: "test.localhost".to_string(),
            port: 4000,
            pid: 12345,
        };

        assert_eq!(route1, route2);
    }

    #[test]
    fn test_route_inequality() {
        let route1 = Route {
            hostname: "test1.localhost".to_string(),
            port: 4000,
            pid: 12345,
        };

        let route2 = Route {
            hostname: "test2.localhost".to_string(),
            port: 4000,
            pid: 12345,
        };

        assert_ne!(route1, route2);
    }
}
