use sol_utils::{
    collections::{
        module_store::ModuleStore,
        vec_map::{VecMap, VecMapIndex},
    },
    fault::Fault,
};
use std::fmt::{Debug, Display};

use crate::fault::display_fault;

pub mod ast;
pub mod benchmark;
pub mod fault;
pub mod mir;
pub mod tokenizer;
pub mod writer;
pub struct PrintConfigs {
    #[cfg(feature = "error_backtrace")]
    pub backtrace: bool,
    pub color: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VecMapEntry<V> {
    id: String,
    value: V,
}

pub fn vecmap_to_pretty_vec<K: VecMapIndex + Debug, V>(map: &VecMap<K, V>) -> Vec<VecMapEntry<&V>> {
    map.entries()
        .map(|(id, value)| VecMapEntry {
            id: format!("{id:?}"),
            value,
        })
        .collect()
}

pub fn vecmap_to_json_str<K, V>(map: &VecMap<K, V>) -> anyhow::Result<String>
where
    K: VecMapIndex + Debug,
    V: serde::Serialize,
{
    let vec = vecmap_to_pretty_vec(map);
    let str = serde_json::to_string_pretty(&vec)?;
    Ok(str)
}

pub fn fault_to_anyhow_error<K: Display>(
    fault: &Fault<K>,
    module_store: &ModuleStore,
    print_configs: &PrintConfigs,
) -> anyhow::Error {
    let mut message = String::new();
    if let Err(err) = display_fault(fault, module_store, print_configs, &mut message) {
        err
    } else {
        anyhow::Error::msg(message)
    }
}
