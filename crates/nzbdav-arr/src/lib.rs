//! Sonarr/Radarr integration and health checking.

pub mod arr_client;
pub mod arr_monitoring;
pub mod error;
pub mod health_check;
pub mod rclone;

pub use arr_client::{ArrClient, ArrConfig};
pub use arr_monitoring::ArrMonitoringService;
pub use health_check::HealthCheckService;
pub use rclone::RcloneClient;
