//! Analysis must keep physical carrier facts distinct from source JS types.
use pd_vm_compat_frontends::javascript::analysis::analyze_javascript_es5_at_path;

#[test]
fn analysis_uses_real_plugin_catalog_and_refuses_unproved_logical_projection() {
    let source = "var JsValue = '😀'; JsValue;";
    let analysis = analyze_javascript_es5_at_path("p11-analysis.js", source).unwrap();
    assert!(analysis.physical_model().diagnostics().is_empty());
    assert_eq!(
        analysis.physical_model().catalog().as_ref(),
        pd_vm_compat_frontends::runtime::cli::reference_array_cli_effective_catalog().as_ref()
    );
    let hints = analysis.physical_type_hints();
    eprintln!("P11_PHYSICAL_HINTS {hints:?}");
    assert!(
        hints
            .iter()
            .any(|hint| matches!(hint.schema, Some(vm::TypeSchema::Named(..))))
    );
    // The public hint producer exposes private helper bindings here and does
    // not issue a source binding/span for this JS name. Do not manufacture one.
    assert!(!hints.iter().any(|hint| hint.name == "JsValue"));
    let error = analysis.logical_type_hints().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("logical JavaScript type projection unavailable")
    );
}

#[test]
fn javascript_analysis_does_not_mix_rss_model_with_js_hints() {
    let error = match analyze_javascript_es5_at_path("p11-analysis.rss", "1;") {
        Ok(_) => panic!("JS analysis must require the JS producer"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("JavaScript ES5 analysis requires a .js source path")
    );
}

#[test]
fn analysis_preserves_actual_admission_error_and_user_span() {
    let source = "var text='😀';\nlet forbidden=1;";
    let error = match analyze_javascript_es5_at_path("p11-span.js", source) {
        Ok(_) => panic!("ES5 grammar must refuse let"),
        Err(error) => error,
    };
    let text = error.to_string();
    eprintln!("P11_ANALYSIS_ERROR {error:?}");
    assert!(text.contains("let"));
    let vm::SourcePathError::SourceWithMap {
        error: vm::SourceError::Parse(parse),
        sources,
    } = error
    else {
        panic!("mapped parse error")
    };
    let span = parse.span.unwrap();
    assert_eq!(&source[span.lo..span.hi], "let forbidden=1;");
    assert_eq!(sources.file(span.source_id).unwrap().name, "p11-span.js");
}
