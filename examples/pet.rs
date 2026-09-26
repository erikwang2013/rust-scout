//! 项目宠物演示：`cargo run --example pet`
//!
//! 打印嗅探猎犬 Scout 的终端横幅，然后牵它去嗅三个**真实**的错误路径
//! （索引名校验、文档字段校验、缺失 feature 的驱动），看它给出什么提示。

use std::io::IsTerminal;

use rust_scout::pet;
use rust_scout::{validate_index_name, EngineManager, ScoutConfig, SearchDocument};

const RESET: &str = "\x1b[0m";
const AMBER: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const DIM: &str = "\x1b[2m";

/// 无新依赖的着色：只有在输出到终端且未设 `NO_COLOR` 时才上色。
fn painter() -> impl Fn(&'static str, &str) -> String {
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    move |code, text| {
        if color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }
}

fn main() {
    let paint = painter();

    println!();
    println!("{}", paint(AMBER, pet::ART));
    println!();
    println!(
        "  {} {}",
        paint(BLUE, pet::NAME),
        paint(DIM, &format!("· {} · rust-scout", pet::SPECIES))
    );
    println!("  {}", paint(DIM, pet::TAGLINE));
    println!();

    println!("{}", paint(BLUE, "── 让它嗅几个真实的错误 ──"));
    println!();

    // 1) 索引名校验
    sniff(
        "validate_index_name(\"a/b\")",
        validate_index_name("a/b").map(|_| ()),
    );

    // 2) 文档字段必须是 JSON 对象
    sniff(
        "SearchDocument::new(\"id\", json!([1, 2, 3]))",
        SearchDocument::new("id", serde_json::json!([1, 2, 3])).map(|_| ()),
    );

    // 3) 驱动缺少 feature（默认 features 下必然失败；全 feature 构建下会成功）
    let driver = match EngineManager::new(ScoutConfig::xunsearch("127.0.0.1:8383", "demo"))
        .engine()
        .map(|_| ())
    {
        Ok(()) => "  xunsearch 驱动已启用 —— 这条路它认得。".to_string(),
        Err(err) => pet::format_error(&err),
    };
    print_outcome("ScoutConfig::xunsearch(..).engine()", &driver);

    println!();
    println!("  {}", paint(DIM, "嗅探结束。爪印留在 docs/svg/pet.svg。"));
    println!();
}

/// 跑一个真实操作，成功画 ✓、失败就把猎犬的提示一并打出来。
fn sniff(label: &str, outcome: rust_scout::Result<()>) {
    let text = match outcome {
        Ok(()) => "\x1b[32m✓ 通过了 —— 这条路上没有错误的味道\x1b[0m".to_string(),
        Err(err) => pet::format_error(&err),
    };
    print_outcome(label, &text);
}

fn print_outcome(label: &str, body: &str) {
    println!("  {label}");
    for line in body.lines() {
        println!("    {line}");
    }
    println!();
}
