/// Minimal VLR representation for temporal index parsing.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct VlrData {
    pub user_id: String,
    pub record_id: u16,
    pub data: Vec<u8>,
}
