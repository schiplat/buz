pub mod local;
pub mod ssh;

use shared::models::task::ExecType;

pub enum EngineType {
    Local,
    Ssh,
    Docker,
    Wasm,
}

impl From<ExecType> for EngineType {
    fn from(t: ExecType) -> Self {
        match t {
            ExecType::Local => EngineType::Local,
            ExecType::Ssh => EngineType::Ssh,
            ExecType::Docker => EngineType::Docker,
            ExecType::Wasm => EngineType::Wasm,
        }
    }
}
