/*! Scans data with already compiled YARA rules.

*/

use crate::compiler::{CompiledRule, CompiledRules, RuleId};
use crate::symbol_table::{SymbolLookup, SymbolTable, TypeValue};
use crate::{modules, wasm};
use bitvec::prelude::*;
use bitvec::vec::BitVec;
use std::ptr::null;
use std::rc::Rc;
use std::slice::Iter;
use wasmtime::{Store, TypedFunc};

#[cfg(test)]
mod tests;

/// Scans data with already compiled YARA rules.
pub struct Scanner<'r> {
    wasm_store: wasmtime::Store<ScanContext<'r>>,
    wasm_main_fn: TypedFunc<(), ()>,
}

impl<'r> Scanner<'r> {
    /// Creates a new scanner.
    pub fn new(compiled_rules: &'r CompiledRules) -> Self {
        let mut wasm_store = Store::new(
            &crate::wasm::ENGINE,
            ScanContext {
                compiled_rules,
                symbol_table: SymbolTable::new(),
                current_struct: None,
                scanned_data: null(),
                scanned_data_len: 0,
                rules_matching: Vec::new(),
                rules_matching_bitmap: BitVec::repeat(
                    false,
                    compiled_rules.rules().len(),
                ),
            },
        );

        let wasm_instance = wasm::LINKER
            .instantiate(&mut wasm_store, compiled_rules.compiled_wasm_mod())
            .unwrap();

        let wasm_main_fn = wasm_instance
            .get_typed_func::<(), (), _>(&mut wasm_store, "main")
            .unwrap();

        Self { wasm_store, wasm_main_fn }
    }

    /// Scans a data buffer.
    pub fn scan(&mut self, data: &[u8]) -> ScanResults {
        let ctx = self.wasm_store.data_mut();

        ctx.rules_matching_bitmap.fill(false);
        ctx.rules_matching.clear();
        ctx.scanned_data = data.as_ptr();
        ctx.scanned_data_len = data.len();

        let imported_modules =
            self.wasm_store.data().compiled_rules.imported_modules();

        for module_name in imported_modules {
            // Lookup the module in the list of built-in modules.
            let module = modules::BUILTIN_MODULES.get(module_name).unwrap();

            // Call the module's main function if any. This function returns
            // a data structure serialized as a protocol buffer. The format of
            // the data is specified by the .proto file associated to the
            // module.
            let module_output = if let Some(main_fn) = module.main_fn {
                main_fn(self.wasm_store.data())
            } else {
                // Implement the case in which the module doesn't have a main
                // function and the serialized data should be provided by the
                // user.
                todo!()
            };

            // Make sure that the module is returning a protobuf message of the
            // expected type.
            debug_assert_eq!(
                module_output.descriptor_dyn().full_name(),
                module.descriptor.full_name(),
                "main function of module `{}` must return `{}`, but returned `{}`",
                module_name,
                module.descriptor.full_name(),
                module_output.descriptor_dyn().full_name(),
            );

            // The data structure obtained from the module is added to the
            // symbol table (data from previous scans is replaced). This
            // structure implements the SymbolLookup trait, which is used
            // by the runtime for obtaining the values of individual fields
            // in the data structure, as they are used in the rule conditions.
            self.wasm_store.data_mut().symbol_table.insert(
                module_name,
                TypeValue::Struct(Rc::new(module_output)),
            );
        }

        // Invoke the main function.
        self.wasm_main_fn.call(&mut self.wasm_store, ()).unwrap();

        let ctx = self.wasm_store.data_mut();

        // Set pointer to data back to nil. This means that accessing
        // `scanned_data` can't be read from within `ScanResults`.
        ctx.scanned_data = null();
        ctx.scanned_data_len = 0;

        ScanResults::new(self.wasm_store.data())
    }
}

/// Results of a scan operation.
pub struct ScanResults<'a> {
    ctx: &'a ScanContext<'a>,
}

impl<'r> ScanResults<'r> {
    fn new(ctx: &'r ScanContext<'r>) -> Self {
        Self { ctx }
    }

    /// Returns the number of rules that matched.
    pub fn matching_rules(&self) -> usize {
        self.ctx.rules_matching.len()
    }

    pub fn iter(&self) -> IterMatches<'r> {
        IterMatches::new(self.ctx)
    }

    pub fn iter_non_matches(&self) -> IterNonMatches<'r> {
        IterNonMatches::new(self.ctx)
    }
}

pub struct IterMatches<'r> {
    ctx: &'r ScanContext<'r>,
    iterator: Iter<'r, RuleId>,
}

impl<'r> IterMatches<'r> {
    fn new(ctx: &'r ScanContext<'r>) -> Self {
        Self { ctx, iterator: ctx.rules_matching.iter() }
    }
}

impl<'r> Iterator for IterMatches<'r> {
    type Item = &'r CompiledRule;

    fn next(&mut self) -> Option<Self::Item> {
        let rule_id = *self.iterator.next()?;
        Some(&self.ctx.compiled_rules.rules()[rule_id as usize])
    }
}

pub struct IterNonMatches<'r> {
    ctx: &'r ScanContext<'r>,
    iterator: bitvec::slice::IterZeros<'r, usize, Lsb0>,
}

impl<'r> IterNonMatches<'r> {
    fn new(ctx: &'r ScanContext<'r>) -> Self {
        Self { ctx, iterator: ctx.rules_matching_bitmap.iter_zeros() }
    }
}

impl<'r> Iterator for IterNonMatches<'r> {
    type Item = &'r CompiledRule;

    fn next(&mut self) -> Option<Self::Item> {
        let rule_id = self.iterator.next()?;
        Some(&self.ctx.compiled_rules.rules()[rule_id])
    }
}

/// Structure that holds information a about the current scan.
pub(crate) struct ScanContext<'r> {
    /// Vector of bits where bit N is set to 1 if the rule with RuleID = N
    /// matched. This is used for determining whether a rule has matched
    /// or not without having to iterate the `rules_matching` vector, and
    /// also for iterating over the non-matching rules in an efficient way.
    pub(crate) rules_matching_bitmap: BitVec,
    /// Vector containing the IDs of the rules that matched.
    pub(crate) rules_matching: Vec<RuleId>,
    /// Data being scanned.
    pub(crate) scanned_data: *const u8,
    /// Length of data being scanned.
    pub(crate) scanned_data_len: usize,
    /// Compiled rules for this scan.
    pub(crate) compiled_rules: &'r CompiledRules,
    /// Symbol table that contains top-level symbols, like module names,
    /// and external variables.
    pub(crate) symbol_table: SymbolTable,
    /// Symbol table for the currently active structure. When this is None
    /// symbols are looked up in `root_sym_tbl` instead.
    pub(crate) current_struct: Option<Rc<dyn SymbolLookup + 'r>>,
}