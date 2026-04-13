
// handles the persistence of room data (e.g. fragments) to disk or other storage
trait FragmentPersistence {
    fn write_fragment(&mut self, room_id: u64, bucket_name: String, fragment_data: Vec<u8>);
    fn read_fragment(&self, room_id: u64, bucket_name: String) -> Option<Vec<u8>>;
}
