use std::{net::SocketAddr, sync::Arc};

use tonic::{transport::Server, Request, Response, Status};
use subtle::ConstantTimeEq;
use crate::state::SharedState;

pub mod proto {
    tonic::include_proto!("syncpond.upstream");
}

use proto::command_service_server::{CommandService, CommandServiceServer};
#[derive(Clone)]
struct CommandServiceImpl {
    inner: Arc<super::CommandServer>,
}

#[tonic::async_trait]
impl CommandService for CommandServiceImpl {
    async fn new_room(
        &self,
        request: Request<proto::NewRoomRequest>,
    ) -> Result<Response<proto::NewRoomResponse>, Status> {
        let req = request.into_inner();
        let name = req.name;

        let resp = self.inner.new_room(name).await;

        Ok(Response::new(proto::NewRoomResponse { room_id: resp }))
    }

    async fn list_rooms(
        &self,
        _request: Request<proto::ListRoomsRequest>,
    ) -> Result<Response<proto::ListRoomsResponse>, Status> {
        let list = self.inner.list_rooms().await;
        Ok(Response::new(proto::ListRoomsResponse { room_ids: list }))
    }

    async fn delete_room(
        &self,
        request: Request<proto::DeleteRoomRequest>,
    ) -> Result<Response<proto::DeleteRoomResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;

        match self.inner.delete_room(room_id).await {
            Ok(()) => Ok(Response::new(proto::DeleteRoomResponse {})),
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn issue_jwt(
        &self,
        request: Request<proto::IssueJwtRequest>,
    ) -> Result<Response<proto::IssueJwtResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let containers = req.containers;

        match self.inner.issue_jwt(room_id, containers).await {
            Ok(token) => Ok(Response::new(proto::IssueJwtResponse { jwt: token })),
            Err(e) => {
                // map known error strings to appropriate gRPC statuses
                match e.as_str() {
                    "room_not_found" => Err(Status::not_found(e)),
                    "reserved_container_name" => Err(Status::invalid_argument(e)),
                    "jwt_key_not_configured" | "jwt_key_too_short" | "jwt_issuer_audience_not_configured" => {
                        Err(Status::failed_precondition(e))
                    }
                    _ => Err(Status::internal(e)),
                }
            }
        }
    }

    async fn create_bucket(
        &self,
        request: Request<proto::CreateBucketRequest>,
    ) -> Result<Response<proto::CreateBucketResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let bucket_id = req.bucket_id;

        match self.inner.create_bucket(room_id, bucket_id).await {
            Ok(()) => Ok(Response::new(proto::CreateBucketResponse {})),
            Err(e) => Err(Status::internal(e)),
        }
    }

    async fn delete_bucket(
        &self,
        request: Request<proto::DeleteBucketRequest>,
    ) -> Result<Response<proto::DeleteBucketResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let bucket_id = req.bucket_id;

        match self.inner.delete_bucket(room_id, bucket_id).await {
            Ok(()) => Ok(Response::new(proto::DeleteBucketResponse {})),
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn list_buckets(
        &self,
        request: Request<proto::ListBucketsRequest>,
    ) -> Result<Response<proto::ListBucketsResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;

        let ids = self.inner.list_buckets(room_id).await;
        Ok(Response::new(proto::ListBucketsResponse { bucket_ids: ids }))
    }

    async fn set_room_label(
        &self,
        request: Request<proto::SetRoomLabelRequest>,
    ) -> Result<Response<proto::SetRoomLabelResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let label = req.label;

        match self.inner.set_room_label(room_id, label).await {
            Ok(()) => Ok(Response::new(proto::SetRoomLabelResponse {})),
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn get_room_label(
        &self,
        request: Request<proto::GetRoomLabelRequest>,
    ) -> Result<Response<proto::GetRoomLabelResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;

        let label = self.inner.get_room_label(room_id).await;
        Ok(Response::new(proto::GetRoomLabelResponse { label: label.unwrap_or_default() }))
    }

    async fn set_bucket_label(
        &self,
        request: Request<proto::SetBucketLabelRequest>,
    ) -> Result<Response<proto::SetBucketLabelResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let bucket_id = req.bucket_id;
        let label = req.label;

        match self.inner.set_bucket_label(room_id, bucket_id, label).await {
            Ok(()) => Ok(Response::new(proto::SetBucketLabelResponse {})),
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn get_bucket_label(
        &self,
        request: Request<proto::GetBucketLabelRequest>,
    ) -> Result<Response<proto::GetBucketLabelResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let bucket_id = req.bucket_id;

        let label = self.inner.get_bucket_label(room_id, bucket_id).await;
        Ok(Response::new(proto::GetBucketLabelResponse { label: label.unwrap_or_default() }))
    }
}

/// A small gRPC server wrapper around the upstream `CommandServer`.
pub struct GrpcServer {
    inner: Arc<super::CommandServer>,
    state: SharedState,
}

impl GrpcServer {
    pub fn new(server: super::CommandServer, state: SharedState) -> Self {
        GrpcServer { inner: Arc::new(server), state }
    }

    /// Serve the gRPC API on the provided address. This registers the generated
    /// protobuf descriptor set for reflection (generated at build time).
    pub async fn serve(self, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        // Read auth-related config once (sync-safe clone for the interceptor)
        let cfg = self.state.read().await;
        let cmd_api_key = cfg.command_api_key.clone();
        drop(cfg);

        // interceptor is synchronous so capture cloned config values
        let api_key_for_interceptor = cmd_api_key.clone();

        let svc = CommandServiceServer::with_interceptor(
            CommandServiceImpl { inner: self.inner.clone() },
            move |req: Request<()>| {
                // Require explicit command API key header `x-syncpond-command-api-key`
                if let Some(cfg_key) = api_key_for_interceptor.as_ref() {
                    if let Some(val) = req.metadata().get("x-syncpond-command-api-key") {
                        if let Ok(vstr) = val.to_str() {
                            if vstr.as_bytes().ct_eq(cfg_key.as_bytes()).into() {
                                return Ok(req);
                            } else {
                                return Err(Status::unauthenticated("invalid api key"));
                            }
                        }
                    }
                }

                Err(Status::unauthenticated("no api key provided"))
            },
        );

        // Descriptor set generated by build.rs in OUT_DIR
        const DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/syncpond_descriptor.pb"));

        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(DESCRIPTOR_SET)
            .build()?;

        Server::builder()
            .add_service(svc)
            .add_service(reflection)
            .serve(addr)
            .await?;

        Ok(())
    }
}
