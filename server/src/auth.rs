use std::collections::HashMap;

// An identity for a client in a room
struct RoomClientIdentity {
    pub id: String, // unique identifier for the client, something the server app will recognize and associate with the client connection
    pub display_name: String, // display name for debuging purposes, not used for authentication
}

struct ClientJwt {
    pub exp: u64,                // expiration time for the token, in seconds since epoch
    pub iat: u64,                // issued at time for the token, in seconds since epoch
    pub nbf: u64,                // not before time for the token, in seconds since epoch
    pub aud: String,             // audience, should match the server's identifier
    pub iss: String,             // issuer, should match the server's identifier
    pub sub: String, // subject, the unique identifier for the client, should match the id in RoomClientIdentity
    pub room: String, // the room the client is trying to access
    pub role: String, // the role of the client, e.g. "editor", "viewer", etc. This can be used for fine-grained permissions within the room
    pub has_command_queue: bool, // whether the client is allowed to send commands to the server. This can be used to implement read-only access for certain clients.
}

struct RoomPermissions {
    pub identities: HashMap<String, RoomClientIdentity>,
}

impl RoomPermissions {
    pub fn new() -> Self {
        RoomPermissions {
            identities: HashMap::new(),
        }
    }

    pub fn add_identity(&mut self, identity: RoomClientIdentity) {
        self.identities.insert(identity.id.clone(), identity);
    }

    pub fn get_identity(&self, id: &str) -> Option<&RoomClientIdentity> {
        self.identities.get(id)
    }

    pub fn remove_identity(&mut self, id: &str) {
        self.identities.remove(id);
    }

    pub fn verify_jwt(&self, jwt: &ClientJwt) -> bool {
        self.identities.contains_key(&jwt.sub)
    }
}

struct Permissions {
    pub rooms: HashMap<String, RoomPermissions>,
}
