pub struct ServiceConfig {
    pub batch_size: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { batch_size: 10 }
    }
}
