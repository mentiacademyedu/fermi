//! System 3: semantic codebase intelligence, against the F-CODE ground truth.

use acceptance_tests::{fixture, ground_truth, tree_hash, Case};
use fermi_core::code::Index;
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// (file, line, end) of a ground-truth identity.
fn def(id: &str) -> (String, u64, u64) {
    let s = ground_truth("F-CODE", "symbols.json");
    let d = s["definitions"].as_array().unwrap().iter().find(|d| d["id"] == id).unwrap_or_else(|| panic!("{id} not in symbols.json"));
    (d["file"].as_str().unwrap().into(), d["line"].as_u64().unwrap(), d["end"].as_u64().unwrap())
}

/// Ground-truth identity at a file and line.
fn identity_at(file: &str, line: u64) -> Option<String> {
    let s = ground_truth("F-CODE", "symbols.json");
    s["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["file"] == file && d["line"].as_u64().unwrap() <= line && line <= d["end"].as_u64().unwrap())
        .map(|d| d["id"].as_str().unwrap().to_string())
}

fn target(id: &str) -> String {
    let (f, l, _) = def(id);
    format!("{f}:{l}")
}

/// The callers fermi reports for an identity, as ground-truth caller ids with lines.
fn callers(idx: &Index, id: &str) -> BTreeSet<(String, String, u64)> {
    let v = idx.symbol(&target(id));
    v["symbols"][0]["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let file = c["caller"]["file"].as_str().unwrap_or("").to_string();
            let line = c["line"].as_u64().unwrap_or(0);
            (identity_at(&file, c["caller"]["line"].as_u64().unwrap_or(0)).unwrap_or_else(|| format!("?{file}")), file, line)
        })
        .collect()
}

fn expected_callers(id: &str) -> BTreeSet<(String, String, u64)> {
    let r = ground_truth("F-CODE", "references.json");
    r["callers"][id]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["caller"].as_str().unwrap().into(), c["file"].as_str().unwrap().into(), c["line"].as_u64().unwrap()))
        .collect()
}

fn open(case: &mut Case) -> (acceptance_tests::Fixture, Index) {
    case.real(&["fermi code index (tree-sitter)", "filesystem", "git"]);
    let fx = fixture("F-CODE", &case.work.join("repo")).expect("fixture");
    let idx = Index::open(&fx.path).expect("index");
    (fx, idx)
}

fn source_untouched(case: &mut Case, fx: &acceptance_tests::Fixture) {
    let now = tree_hash(&acceptance_tests::repo_root().join("tests/fixtures/F-CODE/repo"));
    case.check("groundTruthUnchanged", now == fx.source_hash, format!("fixture source hash {now}"));
}

#[test]
fn ci_01_repository_indexing() {
    let mut c = Case::start("CI-01", "T1");
    let (fx, idx) = open(&mut c);
    let m = ground_truth("F-CODE", "manifest.json");
    let st = idx.stats();
    c.json("index-stats.json", &st);
    let supported: Vec<&str> = m["supportedFiles"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let mut outlines = json!({});
    let mut all_present = true;
    for f in &supported {
        let info = idx.file(f);
        all_present &= info["error"].is_null();
        outlines[*f] = info;
    }
    c.json("file-outlines.json", &outlines);
    c.check("everySupportedFileIndexed", all_present, format!("{} supported files", supported.len()));
    c.check("eachFileOnce", st["files"].as_u64() == Some(supported.len() as u64), format!("index has {} files", st["files"]));
    let unsupported: BTreeSet<String> =
        st["unsupported"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
    let want: BTreeSet<String> = m["unsupportedFiles"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    c.check(
        "unsupportedFilesIdentified",
        want.is_subset(&unsupported),
        format!("index lists unsupported {unsupported:?}, expected {want:?}"),
    );
    c.check(
        "expectedRevision",
        st["revision"].as_str() == Some(fx.commit.as_str()),
        format!("index revision {} vs fixture {}", st["revision"], fx.commit),
    );
    source_untouched(&mut c, &fx);
    c.finish();
}

#[test]
fn ci_02_exact_symbol_lookup() {
    let mut c = Case::start("CI-02", "T1");
    let (fx, idx) = open(&mut c);
    let (file, line, end) = def("engine::describe");
    let hits = idx.search("describe", 10);
    c.json("query-describe.json", &json!(hits));
    let first = hits.first();
    c.check(
        "definitionMatchesOracle",
        first.is_some_and(|h| h.name == "describe" && h.file == file && u64::from(h.line) == line && u64::from(h.end) == end),
        format!("first hit {:?}", first.map(|h| format!("{}:{}-{}", h.file, h.line, h.end))),
    );
    // "normalize" also appears in a comment and a string in app/src/main.rs.
    let norm = idx.search("normalize", 10);
    c.json("query-normalize.json", &json!(norm));
    let (nf, nl, _) = def("util::normalize");
    let exact: Vec<_> = norm.iter().filter(|h| h.name == "normalize").collect();
    c.check(
        "onlyTheDefinition",
        exact.len() == 1 && exact[0].file == nf && u64::from(exact[0].line) == nl,
        format!("{} exact hits", exact.len()),
    );
    c.check("noCommentOrStringHit", !norm.iter().any(|h| h.file == "app/src/main.rs"), "no hit in app/src/main.rs");
    source_untouched(&mut c, &fx);
    c.finish();
}

#[test]
fn ci_03_caller_discovery() {
    let mut c = Case::start("CI-03", "T1");
    let (fx, idx) = open(&mut c);
    let mut report = json!({});
    let mut all = true;
    for id in ["util::normalize", "engine::process", "engine::describe"] {
        let got = callers(&idx, id);
        let want = expected_callers(id);
        all &= got == want;
        report[id] = json!({ "reported": got, "expected": want });
    }
    c.json("caller-comparison.json", &report);
    c.check("knownCallersMatchOracle", all, "callers of normalize, process and describe");
    let v = idx.symbol(&target("util::normalize"));
    c.json("symbol-normalize.json", &v);
    let scope = v["scope"].as_str().unwrap_or("");
    c.check("limitationsDeclared", scope.contains("dynamic"), format!("scope declaration: {scope:?}"));
    source_untouched(&mut c, &fx);
    c.finish();
}

#[test]
fn ci_05_incremental_refresh() {
    let mut c = Case::start("CI-05", "T1");
    let (fx, mut idx) = open(&mut c);
    let before = callers(&idx, "engine::process");
    // describe stops calling process; a new function starts calling it.
    let path = fx.path.join("engine/src/lib.rs");
    let src = std::fs::read_to_string(&path).unwrap();
    let edited = src.replace("    let (n, s) = process(name);", "    let (n, s) = (name.to_string(), 0);")
        + "\n/// Added after indexing.\npub fn shout(name: &str) -> String {\n    process(name).0.to_uppercase()\n}\n";
    // A later mtime, even on filesystems with coarse timestamps.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, &edited).unwrap();
    let stats = idx.update().unwrap();
    let v = idx.symbol("engine/src/lib.rs:5");
    c.json("callers-before.json", &json!(before));
    c.json("symbol-after.json", &v);
    c.check("incremental", stats.parsed == 1, format!("re-parsed {} file(s), no manual rebuild", stats.parsed));
    let names: BTreeSet<String> = v["symbols"][0]["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x["caller"]["name"].as_str().map(String::from))
        .collect();
    c.check("obsoleteReferenceGone", !names.contains("describe"), format!("callers now {names:?}"));
    c.check("newReferencePresent", names.contains("shout") && names.contains("process_scores"), format!("callers now {names:?}"));
    source_untouched(&mut c, &fx);
    c.finish();
}

#[test]
fn ci_07_dependency_impact() {
    let mut c = Case::start("CI-07", "T1");
    let (fx, idx) = open(&mut c);
    let g = ground_truth("F-CODE", "dependency-graph.json");
    let mut ok_all = true;
    let mut out = json!({});
    for id in ["util::normalize", "util::text::ping"] {
        let o = &g["impact"][id];
        let file = o["change"]["file"].as_str().unwrap().to_string();
        let lines = o["change"]["lines"].as_array().unwrap();
        let mut changes = std::collections::BTreeMap::new();
        changes.insert(file, vec![(lines[0].as_u64().unwrap() as u32, lines[1].as_u64().unwrap() as u32)]);
        let imp = idx.impact(&changes);
        let affected: BTreeSet<String> = imp["impacted"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| identity_at(i["symbol"]["file"].as_str()?, i["symbol"]["line"].as_u64()?))
            .collect();
        let want: BTreeSet<String> = o["affectedSymbols"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let sym_ok = want.is_subset(&affected);
        let tests: BTreeSet<String> =
            imp["tests"].as_array().into_iter().flatten().filter_map(|t| identity_at(t["file"].as_str()?, t["line"].as_u64()?)).collect();
        let want_tests: BTreeSet<String> = o["tests"].as_array().into_iter().flatten().map(|v| v.as_str().unwrap().to_string()).collect();
        let test_ok = want_tests.is_subset(&tests);
        let packages: BTreeSet<String> =
            imp["files"].as_array().into_iter().flatten().filter_map(|f| f["file"].as_str()?.split('/').next().map(String::from)).collect();
        let want_pk: BTreeSet<String> =
            o["affectedPackages"].as_array().into_iter().flatten().map(|v| v.as_str().unwrap().to_string()).collect();
        let pk_ok = want_pk.is_subset(&packages);
        ok_all &= sym_ok && test_ok && pk_ok;
        out[id] = json!({ "impact": imp, "affected": affected, "tests": tests, "packages": packages, "symbolsOk": sym_ok, "testsOk": test_ok, "packagesOk": pk_ok });
        if id == "util::normalize" {
            let scope = imp["scope"].as_str().unwrap_or("").to_string();
            c.check("unsupportedRelationshipsDisclosed", scope.contains("dynamic"), format!("scope: {scope:?}"));
        }
    }
    c.json("impact-comparison.json", &out);
    c.check("expectedDependentsAndTests", ok_all, "affected symbols, packages and tests include the oracle sets");
    c.check("cycleTerminates", out["util::text::ping"]["symbolsOk"] == true, "ping <-> pong traversal finished");
    source_untouched(&mut c, &fx);
    c.finish();
}

#[test]
fn ci_08_symbol_identity() {
    let mut c = Case::start("CI-08", "T1");
    let (fx, idx) = open(&mut c);
    let text = callers(&idx, "util::text::helper");
    let num = callers(&idx, "util::num::helper");
    c.json("callers.json", &json!({ "util::text::helper": text, "util::num::helper": num }));
    c.check("textHelperCallers", text == expected_callers("util::text::helper"), format!("{text:?}"));
    c.check("numHelperCallers", num == expected_callers("util::num::helper"), format!("{num:?}"));
    let mut qualified = json!({});
    let mut q_ok = true;
    for id in ["util::text::helper", "util::num::helper"] {
        let v = idx.symbol(id);
        let (file, line, _) = def(id);
        q_ok &= v["matches"] == 1 && v["symbols"][0]["symbol"]["file"] == file && v["symbols"][0]["symbol"]["line"].as_u64() == Some(line);
        qualified[id] = v;
    }
    c.json("qualified-lookups.json", &qualified);
    c.check("qualifiedIdentityResolves", q_ok, "util::text::helper and util::num::helper each resolve to one definition");
    source_untouched(&mut c, &fx);
    let _: Value = Value::Null;
    c.finish();
}
