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

    /// Read a fragment from a specific room/bucket/key.
    /// Returns `Ok(Some(bytes))` when found, `Ok(None)` when absent.
    pub async fn read_fragment(
        &mut self,
        room_id: u64,
        bucket_id: u64,
        key: impl Into<String>,
    ) -> Result<Option<Vec<u8>>, Status> {
        let resp = self.inner.read_fragment(proto::ReadFragmentRequest {
            room_id,
            bucket_id,
            key: key.into(),
        }).await?;
        let inner = resp.into_inner();
        if inner.found {
            Ok(Some(inner.data))
        } else {
            Ok(None)
        }
    }

    /// Write a fragment to a specific room/bucket/key.
    pub async fn write_fragment(
        &mut self,
        room_id: u64,
        bucket_id: u64,
        key: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<(), Status> {
        self.inner.write_fragment(proto::WriteFragmentRequest {
            room_id,
            bucket_id,
            key: key.into(),
            data,
        }).await?;
        Ok(())
    }

    /// Load a room from persistence into memory.
    pub async fn load_room(&mut self, room_id: u64) -> Result<(), Status> {
        self.inner.load_room(proto::LoadRoomRequest { room_id }).await?;
        Ok(())
    }

    /// Unload a room from memory.
    pub async fn unload_room(&mut self, room_id: u64) -> Result<(), Status> {
        self.inner.unload_room(proto::UnloadRoomRequest { room_id }).await?;
        Ok(())
    }
}
