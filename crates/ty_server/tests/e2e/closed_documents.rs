//! Requests for disk-backed targets must not require client document ownership.

use lsp_types::{
    Contents, Definition, DefinitionResponse, Diagnostic, DocumentDiagnosticReport,
    DocumentSymbolParams, DocumentSymbolRequest, DocumentSymbolResponse, ExecuteCommandParams,
    ExecuteCommandRequest, FileChangeType, FileEvent, Position, TextDocumentContentChangeEvent,
    TextDocumentContentChangeWholeDocument, TextDocumentIdentifier, Uri, WorkDoneProgressParams,
};
use ruff_db::system::SystemPath;
use ty_server::{ClientOptions, DiagnosticMode};

use crate::{AwaitResponseError, TestServer, TestServerBuilder};

#[test]
fn server_fulfills_requests_for_closed_workspace_files() {
    let source = "\
def answer() -> int:
    return 42

answer()
";
    // No workspace folders: the server uses its current directory.
    let paths = [
        "main.py",
        "stub.pyi",
        "excluded/file.py",
        "script",
        "script.custom",
    ];
    let mut server = TestServerBuilder::new()
        .expect("test server")
        .with_file(
            "ty.toml",
            r#"[src]
exclude = ["excluded"]
"#,
        )
        .expect("configuration")
        .with_files(paths.map(|path| (path, source)))
        .expect("sources")
        .build()
        .wait_until_workspaces_are_initialized();

    // Test that we support requests against every type of file on disk.
    for path in paths {
        let uri = server.file_uri(path);
        assert_eq!(symbols(&mut server, uri), ["answer"]);
        assert_no_open_documents(&mut server);
    }

    let uri = server.file_uri("main.py");

    // Test a variety of request types that exercise different aspects of the language server
    // (but, for efficiency, only do this part on a single file).
    assert!(
        server
            .hover_request("main.py", Position::new(3, 2))
            .is_some()
    );
    assert!(
        !server
            .semantic_tokens_full_request(&uri)
            .expect("tokens")
            .data
            .is_empty()
    );
    assert!(
        !server
            .folding_range_request(&uri)
            .expect("folds")
            .is_empty()
    );
    assert!(
        server
            .rename(&uri, Position::new(0, 5), "renamed")
            .expect("rename response")
            .is_some()
    );
    assert_no_open_documents(&mut server);
}

#[test]
fn server_uses_current_contents_for_open_and_closed_files() {
    let path = SystemPath::new("deps/library.py");
    let source = "\
value = 'disk'
value
";
    let mut server = dependency_server(false, source);
    assert_hover(&mut server, path, Position::new(1, 0), "Literal[\"disk\"]");

    // Opening the file must replace the cached disk contents with unsaved editor contents.
    let unsaved = "\
value = 'unsaved'
value
";
    server.open_text_document(path, unsaved, 1);
    assert_hover(
        &mut server,
        path,
        Position::new(1, 0),
        "Literal[\"unsaved\"]",
    );
    let edited = "\
value = 'edited'
value
";
    server.change_text_document(
        path,
        vec![
            TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(
                TextDocumentContentChangeWholeDocument {
                    text: edited.into(),
                },
            ),
        ],
        2,
    );
    assert_hover(
        &mut server,
        path,
        Position::new(1, 0),
        "Literal[\"edited\"]",
    );
    // Closing without saving must restore the disk contents.
    server.close_text_document(path);
    assert_hover(&mut server, path, Position::new(1, 0), "Literal[\"disk\"]");

    // While the file is closed, filesystem notifications must refresh the cached contents.
    server.write_file(path, edited).expect("disk edit");
    notify_change(&mut server, path, FileChangeType::Changed);
    assert_hover(
        &mut server,
        path,
        Position::new(1, 0),
        "Literal[\"edited\"]",
    );
    std::fs::remove_file(server.file_path(path)).expect("delete dependency");
    notify_change(&mut server, path, FileChangeType::Deleted);
    assert!(server.hover_request(path, Position::new(1, 0)).is_none());
    server
        .write_file(path, source)
        .expect("recreate dependency");
    notify_change(&mut server, path, FileChangeType::Created);
    assert_hover(&mut server, path, Position::new(1, 0), "Literal[\"disk\"]");
}

#[test]
fn closed_external_file_respects_project_editor_settings() {
    let mut server = dependency_server(
        true,
        "\
value = 42
value
",
    );
    assert!(
        server
            .hover_request("deps/library.py", Position::new(1, 0))
            .is_none()
    );
}

#[test]
fn server_returns_no_closed_file_diagnostics_when_disabled() {
    let mut server = diagnostic_server(DiagnosticMode::Off);

    // Disabling diagnostics also suppresses results for explicitly requested closed files.
    assert!(document_diagnostics(&mut server, "src/main.py").is_empty());
}

#[test]
fn server_returns_no_closed_file_diagnostics_in_open_files_mode() {
    let mut server = diagnostic_server(DiagnosticMode::OpenFilesOnly);

    // An explicit request must not make a closed file count as open.
    assert!(document_diagnostics(&mut server, "src/main.py").is_empty());
}

#[test]
fn server_reports_only_included_closed_files_in_workspace_mode() {
    let mut server = diagnostic_server(DiagnosticMode::Workspace);

    // Workspace mode checks included files even if the client has never opened them.
    let diagnostics = document_diagnostics(&mut server, "src/main.py");
    assert!(diagnostics.iter().any(|diagnostic| {
        matches!(
            &diagnostic.message,
            lsp_types::Message::String(message) if message.contains("undefined_name")
        )
    }));

    // Explicit requests still respect exclusions from type checking.
    assert!(document_diagnostics(&mut server, "src/excluded.py").is_empty());

    // Import search paths allow analysis of external files without enabling their diagnostics.
    assert!(document_diagnostics(&mut server, "deps/library.py").is_empty());

    // Missing files produce empty reports rather than request errors.
    assert!(document_diagnostics(&mut server, "src/missing.py").is_empty());
}

#[test]
fn server_rejects_unsupported_targets() {
    let mut server = TestServerBuilder::new()
        .expect("test server")
        .with_workspace(SystemPath::new("src"), None)
        .expect("workspace")
        .build()
        .wait_until_workspaces_are_initialized();

    for uri in [
        // No project's root or import search paths contain this closed file.
        server.file_uri("outside.py"),
        // Closed notebooks lack the cell URIs and position mappings supplied by the client.
        server.file_uri("src/notebook.ipynb"),
        // An unopened virtual document has neither disk contents nor client-provided contents.
        Uri::parse("untitled:unknown").expect("URI"),
    ] {
        let id = server.send_request::<DocumentSymbolRequest>(symbol_params(uri));
        assert!(
            matches!(server.try_await_response::<DocumentSymbolRequest>(&id, None),
            Err(AwaitResponseError::RequestFailed(error)) if error.code == lsp_server::ErrorCode::InvalidParams as i32)
        );
    }
}

#[test]
fn server_rejects_closed_requests_after_removing_all_workspaces() {
    let workspace = SystemPath::new("src");
    let mut server = TestServerBuilder::new()
        .expect("create test server builder")
        .with_workspace(workspace, None)
        .expect("register workspace")
        .with_file("src/main.py", "value = 42")
        .expect("write source file")
        .build()
        .wait_until_workspaces_are_initialized();

    server.change_workspace_folders([], [workspace]);

    let uri = server.file_uri("src/main.py");
    let id = server.send_request::<DocumentSymbolRequest>(symbol_params(uri.clone()));
    let error = server
        .try_await_response::<DocumentSymbolRequest>(&id, None)
        .expect_err("closed request should fail after removing all workspaces");
    let AwaitResponseError::RequestFailed(error) = error else {
        panic!("expected a request error, got {error:?}");
    };
    assert_eq!(error.code, lsp_server::ErrorCode::InvalidParams as i32);
    assert_eq!(
        error.message,
        format!("Document {uri} is neither open nor a supported closed file")
    );
}

#[test]
fn server_fulfills_requests_for_closed_bundled_stubs() {
    let mut server = TestServerBuilder::new()
        .expect("test server")
        .with_file("main.py", "from enum import StrEnum")
        .expect("source")
        .build()
        .wait_until_workspaces_are_initialized();

    // Go-to-definition gives us the bundled `enum.pyi` URI so we can query the stub
    // without calling `open_text_document` for it.
    let definition = server
        .goto_definition_request("main.py", Position::new(0, 20))
        .expect("StrEnum definition");
    let DefinitionResponse::Definition(Definition::LocationList(locations)) = definition else {
        panic!("expected definition locations");
    };
    let location = locations.first().expect("StrEnum definition location");
    assert!(
        symbols(&mut server, location.uri.clone())
            .iter()
            .any(|name| name == "StrEnum")
    );
    assert_no_open_documents(&mut server);
}

#[track_caller]
fn assert_no_open_documents(server: &mut TestServer) {
    let response = server
        .send_request_await::<ExecuteCommandRequest>(ExecuteCommandParams {
            command: "ty.printDebugInformation".to_string(),
            arguments: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .expect("debug command response");
    let information = response.as_str().expect("debug information string");
    assert_eq!(
        information
            .lines()
            .find(|line| line.starts_with("Open text documents: ")),
        Some("Open text documents: 0"),
        "expected the server to have no open text documents"
    );
}

fn diagnostic_server(mode: DiagnosticMode) -> TestServer {
    let source = "\
import os
undefined_name
";
    TestServerBuilder::new()
        .expect("test server")
        .with_workspace(SystemPath::new("src"), None)
        .expect("workspace")
        .with_initialization_options(&ClientOptions::default().with_diagnostic_mode(mode))
        .with_files([
            (
                "src/ty.toml",
                r#"[src]
exclude = ["excluded.py"]
[environment]
extra-paths = ["../deps"]
"#,
            ),
            ("src/main.py", source),
            ("src/excluded.py", source),
            ("deps/library.py", source),
        ])
        .expect("test files")
        .build()
        .wait_until_workspaces_are_initialized()
}

#[track_caller]
fn document_diagnostics(server: &mut TestServer, path: &str) -> Vec<Diagnostic> {
    let report = server.document_diagnostic_request(path, None);
    let DocumentDiagnosticReport::RelatedFullDocumentDiagnosticReport(report) = report else {
        panic!("expected full diagnostics for {path}");
    };
    report.full_document_diagnostic_report.items
}

fn dependency_server(disable_language_services: bool, source: &str) -> TestServer {
    TestServerBuilder::new()
        .expect("test server")
        .with_workspace(SystemPath::new("a"), None)
        .expect("first workspace")
        .with_workspace(
            SystemPath::new("b/src"),
            Some(
                ClientOptions::default().with_disable_language_services(disable_language_services),
            ),
        )
        .expect("dependency workspace")
        .with_files([
            (
                "b/ty.toml",
                r#"[environment]
extra-paths = ["../deps"]
"#,
            ),
            ("deps/library.py", source),
        ])
        .expect("test files")
        .build()
        .wait_until_workspaces_are_initialized()
}

fn symbol_params(uri: Uri) -> DocumentSymbolParams {
    DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        partial_result_params: lsp_types::PartialResultParams::default(),
    }
}

fn symbols(server: &mut TestServer, uri: Uri) -> Vec<String> {
    match server
        .send_request_await::<DocumentSymbolRequest>(symbol_params(uri))
        .expect("symbols")
    {
        DocumentSymbolResponse::SymbolInformationList(symbols) => symbols
            .into_iter()
            .map(|symbol| symbol.base_symbol_information.name)
            .collect(),
        DocumentSymbolResponse::DocumentSymbolList(symbols) => {
            symbols.into_iter().map(|symbol| symbol.name).collect()
        }
    }
}

fn assert_hover(server: &mut TestServer, path: &SystemPath, position: Position, expected: &str) {
    let hover = server.hover_request(path, position).expect("hover");
    let Contents::MarkupContent(markup) = hover.contents else {
        panic!("expected markup");
    };
    assert_eq!(markup.value, expected);
}

fn notify_change(server: &mut TestServer, path: &SystemPath, kind: FileChangeType) {
    server.did_change_watched_files(vec![FileEvent {
        uri: server.file_uri(path),
        kind,
    }]);
}
