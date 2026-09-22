//! Explicit bound selection lets one call reach either of two declarations of
//! a name at the *same* concrete generic arguments. Where those declarations
//! also share a signature, nothing about the instantiation distinguishes them:
//! the declaration's own identity has to, or independently compiled clients
//! emit one weak symbol for two bodies and the linker silently picks a winner.
//!
//! Every client here is its own `omgc` invocation against one declared
//! provider identity, exactly as separately compiled packages are, and the
//! results are checked by running the linked program in both object orders.
//! This drives the real system toolchain (`cc`, `readelf`); if either is
//! missing the case reports itself as skipped rather than failing.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// Two declarations of `pick` with one signature, and the same again as
/// members of an owner, so the method path is covered as well as the free one.
const PROVIDER: &str = "\
exposed spec A { a_mark(*self) => i32; }\n\
exposed spec B { b_mark(*self) => i32; }\n\
\n\
exposed struct M { exposed value: i32; }\n\
meet A for M { a_mark(*self) => i32 { 1 } }\n\
meet B for M { b_mark(*self) => i32 { 2 } }\n\
\n\
@suppress(unused_parameter)\n\
exposed pick<T: A>(value: T) => i32 { 10 }\n\
@suppress(unused_parameter)\n\
exposed pick<T: B>(value: T) => i32 { 20 }\n\
\n\
exposed struct Holder {\n\
    exposed value: i32;\n\
    @suppress(unused_parameter)\n\
    exposed take<T: A>(*self, thing: T) => i32 { 30 }\n\
    @suppress(unused_parameter)\n\
    exposed take<T: B>(*self, thing: T) => i32 { 40 }\n\
}\n";

/// The same declarations, respelled every way that must not matter: renamed
/// parameters, a reversed declaration order, the bound written through an
/// alias, a reordered conjunction, a default added to a parameter the call
/// fixes anyway, and an unrelated overload alongside.
const RESPELLED_PROVIDER: &str = "\
exposed spec A { a_mark(*self) => i32; }\n\
exposed spec B { b_mark(*self) => i32; }\n\
alias Marked = A;\n\
\n\
exposed struct M { exposed value: i32; }\n\
meet A for M { a_mark(*self) => i32 { 1 } }\n\
meet B for M { b_mark(*self) => i32 { 2 } }\n\
\n\
@suppress(unused_parameter)\n\
exposed pick<U: B>(item: U) => i32 { 20 }\n\
@suppress(unused_parameter)\n\
exposed pick<Value: Marked = M>(item: Value) => i32 { 10 }\n\
@suppress(unused_parameter)\n\
exposed pick<T: B + A>(item: T) => i32 { 30 }\n\
\n\
exposed struct Holder {\n\
    exposed value: i32;\n\
    @suppress(unused_parameter)\n\
    exposed take<U: B>(*self, item: U) => i32 { 40 }\n\
    @suppress(unused_parameter)\n\
    exposed take<Value: Marked>(*self, item: Value) => i32 { 30 }\n\
}\n";

const CLIENT_A: &str = "\
import provider::A;\n\
import provider::M;\n\
import provider::Holder;\n\
import provider::pick;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_a() => i32 { pick<M: A>(M { value = 1; }) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_a_address() => usize {\n\
    selected: (M) => i32 = pick<M: A>;\n\
    <usize><*void>selected\n\
}\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_take_a() => i32 {\n\
    holder := Holder { value = 1; };\n\
    holder.take<M: A>(M { value = 1; })\n\
}\n";

const CLIENT_B: &str = "\
import provider::B;\n\
import provider::M;\n\
import provider::Holder;\n\
import provider::pick;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_b() => i32 { pick<M: B>(M { value = 1; }) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_b_address() => usize {\n\
    selected: (M) => i32 = pick<M: B>;\n\
    <usize><*void>selected\n\
}\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_take_b() => i32 {\n\
    holder := Holder { value = 1; };\n\
    holder.take<M: B>(M { value = 1; })\n\
}\n";

/// Selects the same declaration as `CLIENT_A`, written differently: an
/// inferred `spec ...` selector through a locally declared alias.
const CLIENT_A_AGAIN: &str = "\
import provider::A;\n\
import provider::M;\n\
import provider::pick;\n\
\n\
alias Marker = A;\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_a_again() => i32 { pick<spec Marker>(M { value = 1; }) }\n\
\n\
@symbol(mangle = disabled)\n\
exposed omega_pick_a_again_address() => usize {\n\
    selected: (M) => i32 = pick<spec Marker>;\n\
    <usize><*void>selected\n\
}\n";

const MAIN: &str = "\
int omega_pick_a(void);\n\
unsigned long omega_pick_a_address(void);\n\
int omega_take_a(void);\n\
int omega_pick_b(void);\n\
unsigned long omega_pick_b_address(void);\n\
int omega_take_b(void);\n\
int omega_pick_a_again(void);\n\
unsigned long omega_pick_a_again_address(void);\n\
\n\
int main(void) {\n\
    if (omega_pick_a() != 10) return 1;\n\
    if (omega_pick_b() != 20) return 2;\n\
    if (omega_pick_a_again() != 10) return 3;\n\
    if (omega_take_a() != 30) return 4;\n\
    if (omega_take_b() != 40) return 5;\n\
    if (omega_pick_a_address() != omega_pick_a_again_address()) return 6;\n\
    if (omega_pick_a_address() == omega_pick_b_address()) return 7;\n\
    return 0;\n\
}\n";

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "omgc_generic_overload_linkage_test_{}_{}",
            std::process::id(),
            sequence,
        ));
        let workspace = Self(dir);
        workspace.write_package("provider", "provider", PROVIDER);
        workspace.write_package("clienta", "clienta", CLIENT_A);
        workspace.write_package("clientb", "clientb", CLIENT_B);
        workspace.write_package("clienta2", "clienta2", CLIENT_A_AGAIN);
        fs::write(workspace.0.join("main.c"), MAIN).expect("write harness main");
        workspace
    }

    /// One package root, whose root module is named after its directory.
    fn write_package(&self, dir: &str, module: &str, source: &str) {
        let root = self.0.join(dir);
        fs::create_dir_all(&root).expect("create package root");
        fs::write(root.join(format!("{module}.omg")), source).expect("write root module");
    }

    fn run(&self, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("run {program}: {error}"))
    }

    fn expect_ok(&self, program: &str, args: &[&str]) -> Output {
        let output = self.run(program, args);
        assert!(
            output.status.success(),
            "{program} {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// Compiles one client on its own, against the package identity
    /// `provider` supplied by `provider_root`.
    fn compile_client(&self, client: &str, objects: &str, provider_root: &str) {
        let import = format!("--import=provider:{provider_root}");
        self.expect_ok(
            env!("CARGO_BIN_EXE_omgc"),
            &[client, "-o", objects, &import],
        );
    }

    /// The Omega-mangled symbol names an object defines or references, sorted
    /// so two compilations compare as sets.
    fn omega_symbols(&self, object: &str) -> Vec<String> {
        let table = String::from_utf8_lossy(&self.expect_ok("readelf", &["-sW", object]).stdout)
            .into_owned();
        let mut names: Vec<String> = table
            .lines()
            .filter_map(|line| line.split_whitespace().next_back())
            .filter(|name| name.starts_with("_omg_"))
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        assert!(
            !names.is_empty(),
            "'{object}' defines no mangled symbols, so comparing them proves nothing"
        );
        names
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn independently_compiled_clients_each_reach_the_declaration_they_selected() {
    if !tool_available("cc") {
        eprintln!("skipping: this case needs the system 'cc'");
        return;
    }

    let workspace = Workspace::new();
    workspace.compile_client("clienta", "objects-a", "provider");
    workspace.compile_client("clientb", "objects-b", "provider");
    workspace.compile_client("clienta2", "objects-a2", "provider");

    // Both orders: a symbol collision would be resolved by whichever weak
    // definition the linker saw first, so one order alone could pass.
    for (name, order) in [
        (
            "a-first",
            [
                "objects-a/clienta.o",
                "objects-b/clientb.o",
                "objects-a2/clienta2.o",
            ],
        ),
        (
            "b-first",
            [
                "objects-b/clientb.o",
                "objects-a2/clienta2.o",
                "objects-a/clienta.o",
            ],
        ),
    ] {
        let mut args = vec!["main.c"];
        args.extend(order);
        args.extend(["-o", name]);
        workspace.expect_ok("cc", &args);

        let run = workspace.run(&format!("./{name}"), &[]);
        assert!(
            run.status.success(),
            "{name}: each client must execute the declaration it selected, and two \
             selections of one declaration must be one function; check {:?}",
            run.status.code()
        );
    }

    if !tool_available("readelf") {
        eprintln!("skipping the symbol-table assertions: this needs 'readelf'");
        return;
    }
    let a = workspace.omega_symbols("objects-a/clienta.o");
    let b = workspace.omega_symbols("objects-b/clientb.o");
    let a_again = workspace.omega_symbols("objects-a2/clienta2.o");
    assert!(
        a.iter().all(|symbol| !b.contains(symbol)),
        "two declarations at one instantiation must not share a symbol:\n{a:#?}\n{b:#?}"
    );
    assert!(
        a_again.iter().all(|symbol| a.contains(symbol)),
        "one declaration selected two ways must produce one symbol:\n{a_again:#?}\n{a:#?}"
    );
}

#[test]
fn respelling_a_declaration_does_not_rename_its_instantiations() {
    if !tool_available("readelf") {
        eprintln!("skipping: this case needs the system 'readelf'");
        return;
    }

    let workspace = Workspace::new();
    workspace.write_package("respelled", "respelled", RESPELLED_PROVIDER);
    workspace.compile_client("clienta", "objects-a", "provider");
    workspace.compile_client("clienta", "objects-respelled", "respelled");

    assert_eq!(
        workspace.omega_symbols("objects-a/clienta.o"),
        workspace.omega_symbols("objects-respelled/clienta.o"),
        "parameter names, declaration order, alias spellings, bound order, an \
         added default and an unrelated overload are not declaration identity"
    );
}

#[test]
fn the_declared_package_identity_fixes_the_symbol_not_the_checkout_path() {
    if !tool_available("readelf") {
        eprintln!("skipping: this case needs the system 'readelf'");
        return;
    }

    let workspace = Workspace::new();
    workspace.write_package("elsewhere", "elsewhere", PROVIDER);
    workspace.compile_client("clienta", "objects-a", "provider");
    workspace.compile_client("clienta", "objects-elsewhere", "elsewhere");

    assert_eq!(
        workspace.omega_symbols("objects-a/clienta.o"),
        workspace.omega_symbols("objects-elsewhere/clienta.o"),
        "the declared package identity, not the directory it was found in, is \
         what a descriptor's spec paths name"
    );
}
