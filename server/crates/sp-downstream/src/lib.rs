pub mod auth;
pub mod connection;
pub mod downstream;
pub mod hub;
pub mod server;

pub mod proto {
    tonic::include_proto!("syncpond_downstream");
}

pub use auth::{validate_jwt_claims, Claims};
pub use downstream::Downstream;
pub use hub::WsHub;
pub use server::{spawn_ws_server, start_downstream};
pub use sp_protocol::{ConnectionEvent, DataUpdate};
