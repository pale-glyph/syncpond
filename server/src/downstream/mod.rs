struct Client {
    id: String, // unique identifier for the client (e.g. a UUID)
}

struct WebsocketServer {
    clients: HashMap<String, u64>,
}