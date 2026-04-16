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
    async fn handle(
        &self,
        request: Request<proto::CommandRequest>,
    ) -> Result<Response<proto::CommandResponse>, Status> {
        let req = request.into_inner();

        let proto_cmd = req.command.ok_or_else(|| Status::invalid_argument("missing command"))?;

        let internal_cmd = match proto_cmd {
            proto::command_request::Command::NewRoom(n) => {
                super::Commands::NewRoom(n.name)
            }
            proto::command_request::Command::CloseRoom(c) => {
                super::Commands::CloseRoom(c.room_id)
            }
            proto::command_request::Command::CreateBucket(c) => {
                super::Commands::CreateBucket(c.room_id, c.bucket_id)
            }
            proto::command_request::Command::DeleteBucket(d) => {
                super::Commands::DeleteBucket(d.room_id, d.bucket_id)
            }
            proto::command_request::Command::WriteFragment(w) => {
                super::Commands::WriteFragment(w.room_id, w.fragment_id, w.data)
            }
            proto::command_request::Command::SetFragmentFlags(s) => {
                // convert flags (u32) -> u16 -> FragmentFlags
                let flags = super::FragmentFlags::from_bits(s.flags as u16);
                super::Commands::SetFragmentFlags(s.room_id, s.fragment_id, flags)
            }
            proto::command_request::Command::ReadFragment(r) => {
                super::Commands::ReadFragment(r.room_id, r.fragment_id)
            }
        };

        let resp = self.inner.handle_command(internal_cmd).await;

        let proto_resp = match resp {
            super::CommandResponse::NewRoomResponse(room_id) => proto::CommandResponse {
                response: Some(proto::command_response::Response::NewRoomResponse(
                    proto::NewRoomResponse { room_id },
                )),
            },
            super::CommandResponse::CloseRoomResponse => proto::CommandResponse {
                response: Some(proto::command_response::Response::CloseRoomResponse(
                    proto::CloseRoomResponse {},
                )),
            },
            super::CommandResponse::CreateBucketResponse => proto::CommandResponse {
                response: Some(proto::command_response::Response::CreateBucketResponse(
                    proto::CreateBucketResponse {},
                )),
            },
            super::CommandResponse::DeleteBucketResponse => proto::CommandResponse {
                response: Some(proto::command_response::Response::DeleteBucketResponse(
                    proto::DeleteBucketResponse {},
                )),
            },
            super::CommandResponse::FragmentWriteResponse => proto::CommandResponse {
                response: Some(proto::command_response::Response::FragmentWriteResponse(
                    proto::FragmentWriteResponse {},
                )),
            },
            super::CommandResponse::SetFragmentFlagsResponse => proto::CommandResponse {
                response: Some(proto::command_response::Response::SetFragmentFlagsResponse(
                    proto::SetFragmentFlagsResponse {},
                )),
            },
            super::CommandResponse::FragmentReadResponse(opt) => {
                match opt {
                    Some(data) => proto::CommandResponse {
                        response: Some(proto::command_response::Response::FragmentReadResponse(
                            proto::FragmentReadResponse { data, exists: true },
                        )),
                    },
                    None => proto::CommandResponse {
                        response: Some(proto::command_response::Response::FragmentReadResponse(
                            proto::FragmentReadResponse { data: Vec::new(), exists: false },
                        )),
                    },
                }
            }
        };

        Ok(Response::new(proto_resp))
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
