use async_trait::async_trait;
use std::{net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;
use tonic::{transport::Server, Request, Response, Status};

pub mod proto {
    tonic::include_proto!("syncpond.upstream");
}

use proto::command_service_server::{CommandService, CommandServiceServer};

#[async_trait]
pub trait CommandHandler: Send + Sync + 'static {
    async fn new_room(&self, name: String) -> u64;
    async fn list_rooms(&self) -> Vec<String>;
    async fn delete_room(&self, room_id: u64) -> Result<(), String>;
    async fn issue_jwt(
        &self,
        room_id: u64,
        sub: String,
        buckets: Vec<u64>,
    ) -> Result<String, String>;
    async fn new_bucket(&self, room_id: u64, bucket_id: u64, label: String) -> Result<(), String>;
    async fn delete_bucket(&self, room_id: u64, bucket_id: u64) -> Result<(), String>;
    async fn new_member(&self, room_id: u64, member: String) -> Result<(), String>;
    async fn delete_member(&self, room_id: u64, member: String) -> Result<(), String>;
    async fn list_members(&self, room_id: u64) -> Vec<String>;
    async fn list_buckets(&self, room_id: u64) -> Vec<String>;
    async fn read_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
    ) -> Result<Option<Vec<u8>>, String>;
    async fn write_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
        data: Vec<u8>,
    ) -> Result<(), String>;
    async fn load_room(&self, room_id: u64) -> Result<(), String>;
    async fn unload_room(&self, room_id: u64) -> Result<(), String>;
}

#[derive(Clone)]
struct CommandServiceImpl {
    inner: Arc<dyn CommandHandler>,
}

#[async_trait]
impl CommandService for CommandServiceImpl {
    async fn new_room(
        &self,
        request: Request<proto::NewRoomRequest>,
    ) -> Result<Response<proto::NewRoomResponse>, Status> {
        let req = request.into_inner();
        let room_id = self.inner.new_room(req.name).await;
        Ok(Response::new(proto::NewRoomResponse { room_id }))
    }

    async fn list_rooms(
        &self,
        _request: Request<proto::ListRoomsRequest>,
    ) -> Result<Response<proto::ListRoomsResponse>, Status> {
        let rooms = self.inner.list_rooms().await;
        Ok(Response::new(proto::ListRoomsResponse { rooms }))
    }

    async fn delete_room(
        &self,
        request: Request<proto::DeleteRoomRequest>,
    ) -> Result<Response<proto::DeleteRoomResponse>, Status> {
        let room_id = request.into_inner().room_id;
        self.inner
            .delete_room(room_id)
            .await
            .map(|()| Response::new(proto::DeleteRoomResponse {}))
            .map_err(|e| Status::not_found(e))
    }

    async fn issue_jwt(
        &self,
        request: Request<proto::IssueJwtRequest>,
    ) -> Result<Response<proto::IssueJwtResponse>, Status> {
        let req = request.into_inner();
        match self
            .inner
            .issue_jwt(req.room_id, req.sub, req.buckets)
            .await
        {
            Ok(jwt) => Ok(Response::new(proto::IssueJwtResponse { jwt })),
            Err(e) => match e.as_str() {
                "room_not_found" => Err(Status::not_found(e)),
                "reserved_bucket_id" | "invalid_member" => Err(Status::invalid_argument(e)),
                "jwt_key_not_configured"
                | "jwt_key_too_short"
                | "jwt_issuer_audience_not_configured" => Err(Status::failed_precondition(e)),
                _ => Err(Status::internal(e)),
            },
        }
    }

    async fn new_bucket(
        &self,
        request: Request<proto::NewBucketRequest>,
    ) -> Result<Response<proto::NewBucketResponse>, Status> {
        let req = request.into_inner();
        self.inner
            .new_bucket(req.room_id, req.bucket_id, req.label)
            .await
            .map(|()| Response::new(proto::NewBucketResponse {}))
            .map_err(|e| map_room_error(&e))
    }

    async fn delete_bucket(
        &self,
        request: Request<proto::DeleteBucketRequest>,
    ) -> Result<Response<proto::DeleteBucketResponse>, Status> {
        let req = request.into_inner();
        self.inner
            .delete_bucket(req.room_id, req.bucket_id)
            .await
            .map(|()| Response::new(proto::DeleteBucketResponse {}))
            .map_err(|e| map_room_error(&e))
    }

    async fn new_member(
        &self,
        request: Request<proto::NewMemberRequest>,
    ) -> Result<Response<proto::NewMemberResponse>, Status> {
        let req = request.into_inner();
        self.inner
            .new_member(req.room_id, req.member)
            .await
            .map(|()| Response::new(proto::NewMemberResponse {}))
            .map_err(|e| map_room_error(&e))
    }

    async fn delete_member(
        &self,
        request: Request<proto::DeleteMemberRequest>,
    ) -> Result<Response<proto::DeleteMemberResponse>, Status> {
        let req = request.into_inner();
        self.inner
            .delete_member(req.room_id, req.member)
            .await
            .map(|()| Response::new(proto::DeleteMemberResponse {}))
            .map_err(|e| map_room_error(&e))
    }

    async fn list_members(
        &self,
        request: Request<proto::ListMembersRequest>,
    ) -> Result<Response<proto::ListMembersResponse>, Status> {
        let room_id = request.into_inner().room_id;
        let members = self.inner.list_members(room_id).await;
        Ok(Response::new(proto::ListMembersResponse { members }))
    }

    async fn list_buckets(
        &self,
        request: Request<proto::ListBucketsRequest>,
    ) -> Result<Response<proto::ListBucketsResponse>, Status> {
        let room_id = request.into_inner().room_id;
        let buckets = self.inner.list_buckets(room_id).await;
        Ok(Response::new(proto::ListBucketsResponse { buckets }))
    }

    async fn read_fragment(
        &self,
        request: Request<proto::ReadFragmentRequest>,
    ) -> Result<Response<proto::ReadFragmentResponse>, Status> {
        let req = request.into_inner();
        match self
            .inner
            .read_fragment(req.room_id, req.bucket_id, req.key)
            .await
        {
            Ok(Some(data)) => Ok(Response::new(proto::ReadFragmentResponse {
                data,
                found: true,
            })),
            Ok(None) => Ok(Response::new(proto::ReadFragmentResponse {
                data: vec![],
                found: false,
            })),
            Err(e) => Err(map_room_error(&e)),
        }
    }

    async fn write_fragment(
        &self,
        request: Request<proto::WriteFragmentRequest>,
    ) -> Result<Response<proto::WriteFragmentResponse>, Status> {
        let req = request.into_inner();
        self.inner
            .write_fragment(req.room_id, req.bucket_id, req.key, req.data)
            .await
            .map(|()| Response::new(proto::WriteFragmentResponse {}))
            .map_err(|e| map_room_error(&e))
    }

    async fn load_room(
        &self,
        request: Request<proto::LoadRoomRequest>,
    ) -> Result<Response<proto::LoadRoomResponse>, Status> {
        let room_id = request.into_inner().room_id;
        self.inner
            .load_room(room_id)
            .await
            .map(|()| Response::new(proto::LoadRoomResponse {}))
            .map_err(|e| Status::failed_precondition(e))
    }

    async fn unload_room(
        &self,
        request: Request<proto::UnloadRoomRequest>,
    ) -> Result<Response<proto::UnloadRoomResponse>, Status> {
        let room_id = request.into_inner().room_id;
        self.inner
            .unload_room(room_id)
            .await
            .map(|()| Response::new(proto::UnloadRoomResponse {}))
            .map_err(|e| Status::not_found(e))
    }
}

fn map_room_error(e: &str) -> Status {
    match e {
        "room_not_loaded" => Status::failed_precondition(e),
        "room_not_found" => Status::not_found(e),
        _ => Status::internal(e),
    }
}

pub struct CommandServer {
    inner: Arc<dyn CommandHandler>,
}

impl CommandServer {
    pub fn new(inner: Arc<dyn CommandHandler>) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> Arc<dyn CommandHandler> {
        self.inner
    }
}

pub struct GrpcServer {
    inner: Arc<dyn CommandHandler>,
    api_key: Option<String>,
}

impl GrpcServer {
    pub fn new(server: CommandServer, api_key: Option<String>) -> Self {
        Self {
            inner: server.into_inner(),
            api_key,
        }
    }

    pub async fn serve(self, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        let api_key = self.api_key.clone();

        let svc = CommandServiceServer::with_interceptor(
            CommandServiceImpl {
                inner: self.inner.clone(),
            },
            move |req: Request<()>| {
                if let Some(ref configured_key) = api_key {
                    if let Some(val) = req.metadata().get("x-syncpond-command-api-key") {
                        if let Ok(vstr) = val.to_str() {
                            if vstr.as_bytes().ct_eq(configured_key.as_bytes()).into() {
                                return Ok(req);
                            }
                        }
                    }
                }

                Err(Status::unauthenticated("no api key provided"))
            },
        );

        const DESCRIPTOR_SET: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/syncpond_descriptor.pb"));

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
