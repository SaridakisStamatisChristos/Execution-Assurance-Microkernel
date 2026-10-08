use std::{collections::HashSet, sync::Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    BeforeSnapshot,
    AfterSnapshot,
    BeforeCommit,
    DuringCommit,
    AfterCommit,
    BeforeVerify,
    DuringRollback,
    AfterRollback,
}

impl FaultPoint {
    pub const ALL: [Self; 8] = [
        Self::BeforeSnapshot,
        Self::AfterSnapshot,
        Self::BeforeCommit,
        Self::DuringCommit,
        Self::AfterCommit,
        Self::BeforeVerify,
        Self::DuringRollback,
        Self::AfterRollback,
    ];
}

pub trait FaultInjector: Send + Sync {
    fn hit(&self, point: FaultPoint) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct NoFaultInjector;

impl FaultInjector for NoFaultInjector {
    fn hit(&self, _point: FaultPoint) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ScriptedFaultInjector {
    armed: Mutex<HashSet<FaultPoint>>,
}

impl ScriptedFaultInjector {
    pub fn with_fault(point: FaultPoint) -> Self {
        let mut set = HashSet::new();
        set.insert(point);
        Self {
            armed: Mutex::new(set),
        }
    }
}

impl FaultInjector for ScriptedFaultInjector {
    fn hit(&self, point: FaultPoint) -> Result<(), String> {
        let mut armed = self
            .armed
            .lock()
            .map_err(|_| "fault injector lock poisoned".to_string())?;
        if armed.remove(&point) {
            return Err(format!("{point:?}"));
        }
        Ok(())
    }
}
