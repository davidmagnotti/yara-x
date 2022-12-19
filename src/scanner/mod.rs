/*! Scans data with already compiled YARA rules.

*/

use crate::compiler::{CompiledRule, CompiledRules, RuleId};
use crate::string_pool::BStringPool;
use crate::types::{RuntimeStruct, RuntimeValue};
use crate::{modules, wasm};
use bitvec::prelude::*;
use bitvec::vec::BitVec;
use memmap::MmapOptions;

use std::fs::File;
use std::path::Path;
use std::ptr::null;
use std::rc::Rc;
use std::slice::Iter;
use wasmtime::{
    Global, GlobalType, MemoryType, Mutability, Store, TypedFunc, Val, ValType,
};

#[cfg(test)]
mod tests;

/// Scans data with already compiled YARA rules.
pub struct Scanner<'r> {
    wasm_store: wasmtime::Store<ScanContext<'r>>,
    wasm_main_fn: TypedFunc<(), ()>,
    filesize: wasmtime::Global,
}

impl<'r> Scanner<'r> {
    /// Creates a new scanner.
    pub fn new(compiled_rules: &'r CompiledRules) -> Self {
        let mut wasm_store = Store::new(
            &crate::wasm::ENGINE,
            ScanContext {
                compiled_rules,
                string_pool: BStringPool::new(),
                current_struct: None,
                root_struct: RuntimeStruct::new(),
                scanned_data: null(),
                scanned_data_len: 0,
                rules_matching: Vec::new(),
                rules_matching_bitmap: BitVec::repeat(
                    false,
                    compiled_rules.rules().len(),
                ),
                main_memory: None,
                lookup_stack_top: None,
            },
        );

        // Global variable that will hold the value for `filesize``
        let filesize = Global::new(
            &mut wasm_store,
            GlobalType::new(ValType::I64, Mutability::Var),
            Val::I64(0),
        )
        .unwrap();

        let lookup_stack_top = Global::new(
            &mut wasm_store,
            GlobalType::new(ValType::I32, Mutability::Var),
            Val::I32(0),
        )
        .unwrap();

        let main_memory =
            wasmtime::Memory::new(&mut wasm_store, MemoryType::new(1, None))
                .unwrap();

        // Instantiate the module. This takes the wasm code provided by the
        // `compiled_wasm_mod` function and links its imported functions with
        // the implementations that YARA provides (see wasm.rs).
        let wasm_instance = wasm::new_linker()
            .define("yr", "filesize", filesize)
            .unwrap()
            .define("yr", "lookup_stack_top", lookup_stack_top)
            .unwrap()
            .define("yr", "main_memory", main_memory)
            .unwrap()
            .instantiate(&mut wasm_store, compiled_rules.compiled_wasm_mod())
            .unwrap();

        // Obtain a reference to the "main" function exported by the module.
        let wasm_main_fn = wasm_instance
            .get_typed_func::<(), (), _>(&mut wasm_store, "main")
            .unwrap();

        wasm_store.data_mut().main_memory = Some(main_memory);
        wasm_store.data_mut().lookup_stack_top = Some(lookup_stack_top);

        Self { wasm_store, wasm_main_fn, filesize }
    }

    /// Scans a file.
    pub fn scan_file<'s, P: AsRef<Path>>(
        &'s mut self,
        path: P,
    ) -> std::io::Result<ScanResults<'s, 'r>> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(self.scan(&mmap[..]))
    }

    /// Scans a data buffer.
    pub fn scan<'s>(&'s mut self, data: &[u8]) -> ScanResults<'s, 'r> {
        // Set the global variable `filesize` to the size of the scanned data.
        self.filesize
            .set(&mut self.wasm_store, Val::I64(data.len() as i64))
            .unwrap();

        let ctx = self.wasm_store.data_mut();

        ctx.rules_matching_bitmap.fill(false);
        ctx.rules_matching.clear();
        ctx.scanned_data = data.as_ptr();
        ctx.scanned_data_len = data.len();

        // TODO: this should be done only if the string pool is too large.
        ctx.string_pool = BStringPool::new();

        for module_name in ctx.compiled_rules.imported_modules() {
            // Lookup the module in the list of built-in modules.
            let module = modules::BUILTIN_MODULES.get(module_name).unwrap();

            // Call the module's main function if any. This function returns
            // a data structure serialized as a protocol buffer. The format of
            // the data is specified by the .proto file associated to the
            // module.
            let module_output = if let Some(main_fn) = module.main_fn {
                main_fn(ctx)
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
                module.root_struct_descriptor.full_name(),
                "main function of module `{}` must return `{}`, but returned `{}`",
                module_name,
                module.root_struct_descriptor.full_name(),
                module_output.descriptor_dyn().full_name(),
            );

            // When compile-time optimizations are enabled we don't need to
            // generate structure fields for enums. This is because during the
            // optimization process symbols like MyEnum.ENUM_ITEM are resolved
            // to their constant values at compile time. In other words, the
            // compiler determines that MyEnum.ENUM_ITEM is equal to some value
            // X, and uses that value in the generated code.
            //
            // However, without optimizations, enums are treated as any other
            // field in a struct, and its value is determined at scan time.
            // For that reason these fields must be generated for enums when
            // optimizations are disabled.
            let generate_fields_for_enum =
                !cfg!(feature = "compile-time-optimization");

            let module_struct = RuntimeStruct::from_proto_msg(
                module_output,
                generate_fields_for_enum,
            );

            // The data structure obtained from the module is added to the
            // symbol table (data from previous scans is replaced). This
            // structure implements the SymbolLookup trait, which is used
            // by the runtime for obtaining the values of individual fields
            // in the data structure, as they are used in the rule conditions.
            ctx.root_struct.insert(
                module_name,
                RuntimeValue::Struct(Rc::new(module_struct)),
            );
        }

        // Invoke the main function.
        self.wasm_main_fn.call(&mut self.wasm_store, ()).unwrap();

        let ctx = self.wasm_store.data_mut();

        // Set pointer to data back to nil. This means that accessing
        // `scanned_data` from within `ScanResults` is not possible.
        ctx.scanned_data = null();
        ctx.scanned_data_len = 0;

        ScanResults::new(self.wasm_store.data())
    }
}

/// Results of a scan operation.
pub struct ScanResults<'s, 'r> {
    ctx: &'s ScanContext<'r>,
}

impl<'s, 'r> ScanResults<'s, 'r> {
    fn new(ctx: &'s ScanContext<'r>) -> Self {
        Self { ctx }
    }

    /// Returns the number of rules that matched.
    pub fn matching_rules(&self) -> usize {
        self.ctx.rules_matching.len()
    }

    pub fn iter(&self) -> IterMatches<'s, 'r> {
        IterMatches::new(self.ctx)
    }

    pub fn iter_non_matches(&self) -> IterNonMatches<'s, 'r> {
        IterNonMatches::new(self.ctx)
    }
}

pub struct IterMatches<'s, 'r> {
    ctx: &'s ScanContext<'r>,
    iterator: Iter<'s, RuleId>,
}

impl<'s, 'r> IterMatches<'s, 'r> {
    fn new(ctx: &'s ScanContext<'r>) -> Self {
        Self { ctx, iterator: ctx.rules_matching.iter() }
    }
}

impl<'s, 'r> Iterator for IterMatches<'s, 'r> {
    type Item = &'r CompiledRule;

    fn next(&mut self) -> Option<Self::Item> {
        let rule_id = *self.iterator.next()?;
        Some(&self.ctx.compiled_rules.rules()[rule_id as usize])
    }
}

pub struct IterNonMatches<'s, 'r> {
    ctx: &'s ScanContext<'r>,
    iterator: bitvec::slice::IterZeros<'s, usize, Lsb0>,
}

impl<'s, 'r> IterNonMatches<'s, 'r> {
    fn new(ctx: &'s ScanContext<'r>) -> Self {
        Self { ctx, iterator: ctx.rules_matching_bitmap.iter_zeros() }
    }
}

impl<'s, 'r> Iterator for IterNonMatches<'s, 'r> {
    type Item = &'r CompiledRule;

    fn next(&mut self) -> Option<Self::Item> {
        let rule_id = self.iterator.next()?;
        Some(&self.ctx.compiled_rules.rules()[rule_id])
    }
}

pub(crate) type RuntimeStringId = u32;

/// Structure that holds information a about the current scan.
pub(crate) struct ScanContext<'r> {
    /// Vector of bits where bit N is set to 1 if the rule with RuleId = N
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
    /// Structure that contains top-level symbols, like module names
    /// and external variables. Symbols are normally looked up in this
    /// table, except if `current_struct` is set to some other structure.
    pub(crate) root_struct: RuntimeStruct,
    /// Symbol table for the currently active structure, if any. When this
    /// set it overrides `symbol_table`.
    pub(crate) current_struct: Option<Rc<RuntimeStruct>>,
    /// String pool where the strings produced at runtime are stored. This
    /// for example stores the strings returned by YARA modules.
    pub(crate) string_pool: BStringPool<RuntimeStringId>,
    /// Module's main memory.
    pub(crate) main_memory: Option<wasmtime::Memory>,
    pub(crate) lookup_stack_top: Option<wasmtime::Global>,
}
