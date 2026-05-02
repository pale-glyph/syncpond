use std::{net::SocketAddr, sync::Arc};

use tonic::{transport::Server, Request, Response, Status};
use subtle::ConstantTimeEq;
use crate::state::SharedState;

pub mod proto {
    tonic::include_proto!("syncpond.upstream");
}

use proto::command_service_server::{CommandService, CommandServiceServer};

/// Map a kernel error string to the appropriate gRPC `Status`.
/// `room_not_loaded` → `failed_precondition`; `room_not_found` → `not_found`; others → `internal`.
fn map_room_error(e: &str) -> Status {
    match e {
        "room_not_loaded" => Status::failed_precondition(e),
        "room_not_found" => Status::not_found(e),
        _ => Status::internal(e),
    }
}

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
        let entries = self.inner.list_rooms().await;
        Ok(Response::new(proto::ListRoomsResponse { rooms: entries }))
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
        let sub = req.sub;
        let buckets = req.buckets;

        match self.inner.issue_jwt(room_id, sub, buckets).await {
            Ok(token) => Ok(Response::new(proto::IssueJwtResponse { jwt: token })),
            Err(e) => {
                // map known error strings to appropriate gRPC statuses
                match e.as_str() {
                    "room_not_found" => Err(Status::not_found(e)),
                    "reserved_bucket_id" | "invalid_member" => Err(Status::invalid_argument(e)),
                    "jwt_key_not_configured" | "jwt_key_too_short" | "jwt_issuer_audience_not_configured" => {
                        Err(Status::failed_precondition(e))
                    }
                    _ => Err(Status::internal(e)),
                }
            }
        }
    }

    async fn new_bucket(
        &self,
        request: Request<proto::NewBucketRequest>,
    ) -> Result<Response<proto::NewBucketResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let bucket_id = req.bucket_id;
        let label = req.label;

        match self.inner.new_bucket(room_id, bucket_id, label).await {
            Ok(()) => Ok(Response::new(proto::NewBucketResponse {})),
            Err(e) => Err(map_room_error(&e)),
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
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn new_member(
        &self,
        request: Request<proto::NewMemberRequest>,
    ) -> Result<Response<proto::NewMemberResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let member = req.member;

        match self.inner.new_member(room_id, member).await {
            Ok(()) => Ok(Response::new(proto::NewMemberResponse {})),
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn delete_member(
        &self,
        request: Request<proto::DeleteMemberRequest>,
    ) -> Result<Response<proto::DeleteMemberResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;
        let member = req.member;

        match self.inner.delete_member(room_id, member).await {
            Ok(()) => Ok(Response::new(proto::DeleteMemberResponse {})),
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn list_members(
        &self,
        request: Request<proto::ListMembersRequest>,
    ) -> Result<Response<proto::ListMembersResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;

        let entries = self.inner.list_members(room_id).await;
        Ok(Response::new(proto::ListMembersResponse { members: entries }))
    }

    async fn list_buckets(
        &self,
        request: Request<proto::ListBucketsRequest>,
    ) -> Result<Response<proto::ListBucketsResponse>, Status> {
        let req = request.into_inner();
        let room_id = req.room_id;

        let entries = self.inner.list_buckets(room_id).await;
        Ok(Response::new(proto::ListBucketsResponse { buckets: entries }))
    }

    async fn read_fragment(
        &self,
        request: Request<proto::ReadFragmentRequest>,
    ) -> Result<Response<proto::ReadFragmentResponse>, Status> {
        let req = request.into_inner();
        match self.inner.read_fragment(req.room_id, req.bucket_id, req.key).await {
            Ok(Some(data)) => Ok(Response::new(proto::ReadFragmentResponse { data, found: true })),
            Ok(None) => Ok(Response::new(proto::ReadFragmentResponse { data: vec![], found: false })),
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn write_fragment(
        &self,
        request: Request<proto::WriteFragmentRequest>,
    ) -> Result<Response<proto::WriteFragmentResponse>, Status> {
        let req = request.into_inner();
        match self.inner.write_fragment(req.room_id, req.bucket_id, req.key, req.data).await {
            Ok(()) => Ok(Response::new(proto::WriteFragmentResponse {})),
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn load_room(
        &self,
        request: Request<proto::LoadRoomRequest>,
    ) -> Result<Response<proto::LoadRoomResponse>, Status> {
        let req = request.into_inner();
        match self.inner.load_room(req.room_id).await {
            Ok(()) => Ok(Response::new(proto::LoadRoomResponse {})),
            Err(e) => Err(Status::failed_precondition(e)),
        }
    }

    async fn unload_room(
        &self,
        request: Request<proto::UnloadRoomRequest>,
    ) -> Result<Response<proto::UnloadRoomResponse>, Status> {
        let req = request.into_inner();
        match self.inner.unload_room(req.room_id).await {
            Ok(()) => Ok(Response::new(proto::UnloadRoomResponse {})),
            Err(e) => Err(Status::not_found(e)),
        }
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
