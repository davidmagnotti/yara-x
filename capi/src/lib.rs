/*! C bindings for the YARA-X library.

This crate defines the C-compatible API that C/C++ programs can use for
interfacing with the YARA-X Rust library. A header file for this library
(`yara_x.h`) will be automatically generated by [`cbindgen`][1], during
compilation, together with dynamic-linking and static-linking versions of
the library.

# How to build and install

You will need [`cargo-c`][2] for building this library, if you didn't install
it before, this is the first step:
```text
cargo install cargo-c
```

You will also need the `openssl` library, depending on your platform you
can choose one of the following methods:

Ubuntu:

```text
sudo apt install libssl-dev
```

MacOS (using [`brew`][3]):

```text
brew install openssl@3
```

Windows (using [`vcpkg`][4]):

```text
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
bootstrap-vcpkg.bat
vcpkg install openssl:x64-windows-static
set OPENSSL_DIR=%cd%\installed\x64-windows-static
```

Once you have installed the pre-requisites, go to the root directory
of the YARA-X repository and type:

```text
cargo cinstall -p yara-x-capi --release
```

The command above will put the library and header files in the correct path
in your system (usually `/usr/local/lib` and `/usr/local/include` for Linux
and MacOS users), and will generate a `.pc` file so that `pkg-config` knows
about the library.

In Linux and MacOS you can check if everything went fine by compiling a simple
test program, like this:

```text
cat <<EOF > test.c
#include <yara_x.h>
int main() {
    YRX_RULES* rules;
    yrx_compile("rule dummy { condition: true }", &rules);
    yrx_rules_destroy(rules);
}
EOF
```

```text
gcc `pkg-config --cflags yara_x_capi` `pkg-config --libs yara_x_capi` test.c
```

The compilation should succeed without errors.

Windows users can find all the files you need for importing the YARA-X library
in your project in the `target/x86_64-pc-windows-msvc/release` directory. This
includes:

* A header file (`yara_x.h`)
* A [module definition file][4] (`yara_x_capi.def`)
* A DLL file (`yara_x_capi.dll`) with its corresponding import library (`yara_x_capi.dll.lib`)
* A static library (`yara_x_capi.lib`)


[1]: https://github.com/mozilla/cbindgen
[2]: https://github.com/lu-zero/cargo-c
[3]: https://brew.sh
[4]: https://vcpkg.io/
[4]: https://learn.microsoft.com/en-us/cpp/build/reference/module-definition-dot-def-files
 */

#![deny(missing_docs)]
#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::mem::ManuallyDrop;
use std::ptr::slice_from_raw_parts_mut;
use std::slice;

mod compiler;
mod scanner;

#[cfg(test)]
mod tests;

pub use scanner::*;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Error codes returned by functions in this API.
#[repr(C)]
pub enum YRX_RESULT {
    /// Everything was OK.
    SUCCESS,
    /// A syntax error occurred while compiling YARA rules.
    SYNTAX_ERROR,
    /// An error occurred while defining or setting a global variable. This may
    /// happen when a variable is defined twice and when you try to set a value
    /// that doesn't correspond to the variable's type.
    VARIABLE_ERROR,
    /// An error occurred during a scan operation.
    SCAN_ERROR,
    /// A scan operation was aborted due to a timeout.
    SCAN_TIMEOUT,
    /// An error indicating that some of the arguments passed to a function is
    /// invalid. Usually indicates a nil pointer to a scanner or compiler.
    INVALID_ARGUMENT,
    /// An error indicating that some of the strings passed to a function is
    /// not valid UTF-8.
    INVALID_UTF8,
    /// An error occurred while serializing/deserializing YARA rules.
    SERIALIZATION_ERROR,
    /// An error returned when a rule doesn't have any metadata.
    NO_METADATA,
}

/// A set of compiled YARA rules.
pub struct YRX_RULES(yara_x::Rules);

/// A single YARA rule.
pub struct YRX_RULE<'a, 'r>(yara_x::Rule<'a, 'r>);

/// Represents the metadata associated to a rule.
#[repr(C)]
pub struct YRX_METADATA {
    /// Number of metadata entries.
    num_entries: usize,
    /// Pointer to an array of YRX_METADATA_ENTRY structures. The array has
    /// num_entries items. If num_entries is zero this pointer is invalid
    /// and should not be de-referenced.
    entries: *mut YRX_METADATA_ENTRY,
}

impl Drop for YRX_METADATA {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(slice_from_raw_parts_mut(
                self.entries,
                self.num_entries,
            )));
        }
    }
}

/// Metadata value types.
#[repr(C)]
#[allow(missing_docs)]
pub enum YRX_METADATA_VALUE_TYPE {
    INTEGER,
    FLOAT,
    BOOLEAN,
    STRING,
    BYTES,
}

/// Represents a metadata value that contains raw bytes.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct YRX_METADATA_BYTES {
    /// Number of bytes.
    length: usize,
    /// Pointer to the bytes.
    data: *mut u8,
}

/// Metadata value.
#[repr(C)]
union YRX_METADATA_VALUE {
    integer: i64,
    float: f64,
    boolean: bool,
    string: *mut c_char,
    bytes: YRX_METADATA_BYTES,
}

/// A metadata entry.
#[repr(C)]
pub struct YRX_METADATA_ENTRY {
    /// Metadata identifier.
    identifier: *mut c_char,
    /// Type of value.
    value_type: YRX_METADATA_VALUE_TYPE,
    /// The value itself. This is a union, use the member that matches the
    /// value type.
    value: YRX_METADATA_VALUE,
}

impl Drop for YRX_METADATA_ENTRY {
    fn drop(&mut self) {
        unsafe {
            drop(CString::from_raw(self.identifier));
            match self.value_type {
                YRX_METADATA_VALUE_TYPE::STRING => {
                    drop(CString::from_raw(self.value.string));
                }
                YRX_METADATA_VALUE_TYPE::BYTES => {
                    drop(Box::from_raw(slice_from_raw_parts_mut(
                        self.value.bytes.data,
                        self.value.bytes.length,
                    )));
                }
                _ => {}
            }
        }
    }
}

/// A set of patterns declared in a YARA rule.
#[repr(C)]
pub struct YRX_PATTERNS {
    /// Number of patterns.
    num_patterns: usize,
    /// Pointer to an array of YRX_PATTERN structures. The array has
    /// num_patterns items. If num_patterns is zero this pointer is invalid
    /// and should not be de-referenced.
    patterns: *mut YRX_PATTERN,
}

impl Drop for YRX_PATTERNS {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(slice_from_raw_parts_mut(
                self.patterns,
                self.num_patterns,
            )));
        }
    }
}

/// A pattern within a rule.
#[repr(C)]
pub struct YRX_PATTERN {
    /// Pattern's identifier (i.e: $a, $foo)
    identifier: *mut c_char,
    /// Number of matches found for this pattern.
    num_matches: usize,
    /// Pointer to an array of YRX_MATCH structures describing the matches
    /// for this pattern. The array has num_matches items. If num_matches is
    /// zero this pointer is invalid and should not be de-referenced.
    matches: *mut YRX_MATCH,
}

impl Drop for YRX_PATTERN {
    fn drop(&mut self) {
        unsafe {
            drop(CString::from_raw(self.identifier));
            drop(Box::from_raw(slice_from_raw_parts_mut(
                self.matches,
                self.num_matches,
            )));
        }
    }
}

/// Contains information about a pattern match.
#[repr(C)]
pub struct YRX_MATCH {
    /// Offset within the data where the match occurred.
    pub offset: usize,
    /// Length of the match.
    pub length: usize,
}

/// Represents a buffer with arbitrary data.
#[repr(C)]
pub struct YRX_BUFFER {
    /// Pointer to the data contained in the buffer.
    pub data: *mut u8,
    /// Length of data in bytes.
    pub length: usize,
}

impl Drop for YRX_BUFFER {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(slice_from_raw_parts_mut(
                self.data,
                self.length,
            )));
        }
    }
}

/// Compiles YARA source code and creates a [`YRX_RULES`] object that contains
/// the compiled rules.
///
/// The rules must be destroyed with [`yrx_rules_destroy`].
#[no_mangle]
pub unsafe extern "C" fn yrx_compile(
    src: *const c_char,
    rules: &mut *mut YRX_RULES,
) -> YRX_RESULT {
    let c_str = CStr::from_ptr(src);
    match yara_x::compile(c_str.to_bytes()) {
        Ok(r) => {
            *rules = Box::into_raw(Box::new(YRX_RULES(r)));
            LAST_ERROR.set(None);
            YRX_RESULT::SUCCESS
        }
        Err(err) => {
            LAST_ERROR.set(Some(CString::new(err.to_string()).unwrap()));
            YRX_RESULT::SYNTAX_ERROR
        }
    }
}

/// Serializes the rules as a sequence of bytes.
///
/// In the address indicated by the `buf` pointer, the function will copy a
/// `YRX_BUFFER*` pointer. The `YRX_BUFFER` structure represents a buffer
/// that contains the serialized rules. This structure has a pointer to the
/// data itself, and its length.
///
/// The [`YRX_BUFFER`] must be destroyed with [`yrx_buffer_destroy`].
#[no_mangle]
pub unsafe extern "C" fn yrx_rules_serialize(
    rules: *mut YRX_RULES,
    buf: &mut *mut YRX_BUFFER,
) -> YRX_RESULT {
    if let Some(rules) = rules.as_ref() {
        match rules.0.serialize() {
            Ok(serialized) => {
                let serialized = serialized.into_boxed_slice();
                let mut serialized = ManuallyDrop::new(serialized);
                *buf = Box::into_raw(Box::new(YRX_BUFFER {
                    data: serialized.as_mut_ptr(),
                    length: serialized.len(),
                }));
                LAST_ERROR.set(None);
                YRX_RESULT::SUCCESS
            }
            Err(err) => {
                LAST_ERROR.set(Some(CString::new(err.to_string()).unwrap()));
                YRX_RESULT::SERIALIZATION_ERROR
            }
        }
    } else {
        YRX_RESULT::INVALID_ARGUMENT
    }
}

/// Deserializes the rules from a sequence of bytes produced by
/// [`yrx_rules_serialize`].
///
#[no_mangle]
pub unsafe extern "C" fn yrx_rules_deserialize(
    data: *const u8,
    len: usize,
    rules: &mut *mut YRX_RULES,
) -> YRX_RESULT {
    match yara_x::Rules::deserialize(slice::from_raw_parts(data, len)) {
        Ok(r) => {
            *rules = Box::into_raw(Box::new(YRX_RULES(r)));
            LAST_ERROR.set(None);
            YRX_RESULT::SUCCESS
        }
        Err(err) => {
            LAST_ERROR.set(Some(CString::new(err.to_string()).unwrap()));
            YRX_RESULT::SERIALIZATION_ERROR
        }
    }
}

/// Destroys a [`YRX_RULES`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_rules_destroy(rules: *mut YRX_RULES) {
    drop(Box::from_raw(rules))
}

/// Returns the name of the rule represented by [`YRX_RULE`].
///
/// Arguments `ident` and `len` are output parameters that receive pointers
/// to a `const uint8_t*` and `size_t`, where this function will leave a pointer
/// to the rule's name and its length, respectively. The rule's name is *NOT*
/// null-terminated, and the pointer will be valid as long as the [`YRX_RULES`]
/// object that contains the rule is not freed. The name is guaranteed to be a
/// valid UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yrx_rule_identifier(
    rule: *const YRX_RULE,
    ident: &mut *const u8,
    len: &mut usize,
) -> YRX_RESULT {
    if let Some(rule) = rule.as_ref() {
        *ident = rule.0.identifier().as_ptr();
        *len = rule.0.identifier().len();
        LAST_ERROR.set(None);
        YRX_RESULT::SUCCESS
    } else {
        YRX_RESULT::INVALID_ARGUMENT
    }
}

/// Returns the namespace of the rule represented by [`YRX_RULE`].
///
/// Arguments `ns` and `len` are output parameters that receive pointers to a
/// `const uint8_t*` and `size_t`, where this function will leave a pointer
/// to the rule's namespace and its length, respectively. The namespace is *NOT*
/// null-terminated, and the pointer will be valid as long as the [`YRX_RULES`]
/// object that contains the rule is not freed. The namespace is guaranteed to
/// be a valid UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn yrx_rule_namespace(
    rule: *const YRX_RULE,
    ns: &mut *const u8,
    len: &mut usize,
) -> YRX_RESULT {
    if let Some(rule) = rule.as_ref() {
        *ns = rule.0.namespace().as_ptr();
        *len = rule.0.namespace().len();
        LAST_ERROR.set(None);
        YRX_RESULT::SUCCESS
    } else {
        YRX_RESULT::INVALID_ARGUMENT
    }
}

/// Returns the metadata associated to a rule.
///
/// The metadata is represented by a [`YRX_METADATA`] object that must be
/// destroyed with [`yrx_metadata_destroy`] when not needed anymore.
///
/// This function returns a null pointer when `rule` is null or the
/// rule doesn't have any metadata.
pub unsafe extern "C" fn yrx_rule_metadata(
    rule: *const YRX_RULE,
) -> *mut YRX_METADATA {
    let metadata = if let Some(rule) = rule.as_ref() {
        rule.0.metadata()
    } else {
        return std::ptr::null_mut();
    };

    if metadata.is_empty() {
        return std::ptr::null_mut();
    }

    let mut entries = Vec::with_capacity(metadata.len());

    for (identifier, value) in metadata {
        let identifier = CString::new(identifier).unwrap().into_raw();

        match value {
            yara_x::MetaValue::Integer(v) => {
                entries.push(YRX_METADATA_ENTRY {
                    identifier,
                    value_type: YRX_METADATA_VALUE_TYPE::INTEGER,
                    value: YRX_METADATA_VALUE { integer: v },
                });
            }
            yara_x::MetaValue::Float(v) => {
                entries.push(YRX_METADATA_ENTRY {
                    identifier,
                    value_type: YRX_METADATA_VALUE_TYPE::FLOAT,
                    value: YRX_METADATA_VALUE { float: v },
                });
            }
            yara_x::MetaValue::Bool(v) => {
                entries.push(YRX_METADATA_ENTRY {
                    identifier,
                    value_type: YRX_METADATA_VALUE_TYPE::BOOLEAN,
                    value: YRX_METADATA_VALUE { boolean: v },
                });
            }
            yara_x::MetaValue::String(v) => {
                entries.push(YRX_METADATA_ENTRY {
                    identifier,
                    value_type: YRX_METADATA_VALUE_TYPE::STRING,
                    value: YRX_METADATA_VALUE {
                        string: CString::new(v).unwrap().into_raw(),
                    },
                });
            }
            yara_x::MetaValue::Bytes(v) => {
                let v = v.to_vec().into_boxed_slice();
                let mut v = ManuallyDrop::new(v);
                entries.push(YRX_METADATA_ENTRY {
                    identifier,
                    value_type: YRX_METADATA_VALUE_TYPE::BYTES,
                    value: YRX_METADATA_VALUE {
                        bytes: YRX_METADATA_BYTES {
                            data: v.as_mut_ptr(),
                            length: v.len(),
                        },
                    },
                });
            }
        };
    }

    let mut entries = ManuallyDrop::new(entries);

    Box::into_raw(Box::new(YRX_METADATA {
        num_entries: entries.len(),
        entries: entries.as_mut_ptr(),
    }))
}

/// Destroys a [`YRX_METADATA`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_metadata_destroy(metadata: *mut YRX_METADATA) {
    drop(Box::from_raw(metadata));
}

/// Returns all the patterns defined by a rule.
///
/// Each pattern contains information about whether it matched or not, and where
/// in the data it matched. The patterns are represented by a [`YRX_PATTERNS`]
/// object that must be destroyed with [`yrx_patterns_destroy`] when not needed
/// anymore.
///
/// This function returns a null pointer when `rule` is null.
#[no_mangle]
pub unsafe extern "C" fn yrx_rule_patterns(
    rule: *const YRX_RULE,
) -> *mut YRX_PATTERNS {
    let patterns_iter = if let Some(rule) = rule.as_ref() {
        rule.0.patterns()
    } else {
        return std::ptr::null_mut();
    };

    let mut patterns = Vec::with_capacity(patterns_iter.len());

    for pattern in patterns_iter {
        let matches = pattern
            .matches()
            .map(|m| YRX_MATCH {
                offset: m.range().start,
                length: m.range().len(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        // Prevent `matches` from being dropped at the end of the current
        // scope. We are taking a pointer to `matches` and storing it in a
        // YRX_PATTERN structure. The `YRX_PATTERN::drop` method takes care
        // of dropping the slice of matches.
        let mut matches = ManuallyDrop::new(matches);

        patterns.push(YRX_PATTERN {
            identifier: CString::new(pattern.identifier()).unwrap().into_raw(),
            num_matches: matches.len(),
            matches: matches.as_mut_ptr(),
        });
    }

    let mut patterns = ManuallyDrop::new(patterns);

    Box::into_raw(Box::new(YRX_PATTERNS {
        num_patterns: patterns.len(),
        patterns: patterns.as_mut_ptr(),
    }))
}

/// Destroys a [`YRX_PATTERNS`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_patterns_destroy(patterns: *mut YRX_PATTERNS) {
    drop(Box::from_raw(patterns));
}

/// Destroys a [`YRX_BUFFER`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_buffer_destroy(buf: *mut YRX_BUFFER) {
    drop(Box::from_raw(buf));
}

/// Returns the error message for the most recent function in this API
/// invoked by the current thread.
///
/// The returned pointer is only valid until this thread calls some other
/// function, as it can modify the last error and render the pointer to
/// a previous error message invalid. Also, the pointer will be null if
/// the most recent function was successfully.
#[no_mangle]
pub unsafe extern "C" fn yrx_last_error() -> *const c_char {
    LAST_ERROR.with_borrow(|last_error| {
        if let Some(last_error) = last_error {
            last_error.as_ptr()
        } else {
            std::ptr::null()
        }
    })
}
