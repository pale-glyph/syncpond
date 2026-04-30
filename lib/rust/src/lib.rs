mod proto {
    tonic::include_proto!("syncpond.upstream");
}

use proto::command_service_client::CommandServiceClient;
use tonic::transport::Channel;

pub use tonic::Status;
pub use tonic::transport::Error as TransportError;

/// gRPC client for the Syncpond upstream `CommandService`.
pub struct SyncpondClient {
    inner: CommandServiceClient<Channel>,
}

impl SyncpondClient {
    /// Connect to the Syncpond server at the given address (e.g. `"http://127.0.0.1:50051"`).
    pub async fn connect(addr: impl Into<String>) -> Result<Self, TransportError> {
        let inner = CommandServiceClient::connect(addr.into()).await?;
        Ok(Self { inner })
    }

    pub async fn new_room(&mut self, name: impl Into<String>) -> Result<u64, Status> {
        let resp = self.inner.new_room(proto::NewRoomRequest { name: name.into() }).await?;
        Ok(resp.into_inner().room_id)
    }

    pub async fn list_rooms(&mut self) -> Result<Vec<String>, Status> {
        let resp = self.inner.list_rooms(proto::ListRoomsRequest {}).await?;
        Ok(resp.into_inner().rooms)
    }

    pub async fn delete_room(&mut self, room_id: u64) -> Result<(), Status> {
        self.inner.delete_room(proto::DeleteRoomRequest { room_id }).await?;
        Ok(())
    }

    pub async fn issue_jwt(
        &mut self,
        room_id: u64,
        sub: impl Into<String>,
        buckets: Vec<u64>,
    ) -> Result<String, Status> {
        let resp = self.inner.issue_jwt(proto::IssueJwtRequest {
            room_id,
            sub: sub.into(),
            buckets,
        }).await?;
        Ok(resp.into_inner().jwt)
    }

    pub async fn new_bucket(
        &mut self,
        room_id: u64,
        bucket_id: u64,
        label: impl Into<String>,
    ) -> Result<(), Status> {
        self.inner.new_bucket(proto::NewBucketRequest {
            room_id,
            bucket_id,
            label: label.into(),
        }).await?;
        Ok(())
    }

    pub async fn delete_bucket(&mut self, room_id: u64, bucket_id: u64) -> Result<(), Status> {
        self.inner.delete_bucket(proto::DeleteBucketRequest { room_id, bucket_id }).await?;
        Ok(())
    }

    pub async fn list_buckets(&mut self, room_id: u64) -> Result<Vec<String>, Status> {
        let resp = self.inner.list_buckets(proto::ListBucketsRequest { room_id }).await?;
        Ok(resp.into_inner().buckets)
    }

    pub async fn new_member(
        &mut self,
        room_id: u64,
        member: impl Into<String>,
    ) -> Result<(), Status> {
        self.inner.new_member(proto::NewMemberRequest {
            room_id,
            member: member.into(),
        }).await?;
        Ok(())
    }

    pub async fn delete_member(
        &mut self,
        room_id: u64,
        member: impl Into<String>,
    ) -> Result<(), Status> {
        self.inner.delete_member(proto::DeleteMemberRequest {
            room_id,
            member: member.into(),
        }).await?;
        Ok(())
    }

    pub async fn list_members(&mut self, room_id: u64) -> Result<Vec<String>, Status> {
        let resp = self.inner.list_members(proto::ListMembersRequest { room_id }).await?;
        Ok(resp.into_inner().members)
    }
}
