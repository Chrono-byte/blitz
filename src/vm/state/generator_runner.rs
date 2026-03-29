use super::Vm;

pub(crate) struct GeneratorRunner {
    pub(crate) vm: Vm,
    pub(crate) started: bool,
    pub(crate) completed: bool,
}
