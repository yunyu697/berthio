mod demo;
mod error;
mod export;
mod http;
mod kernels;
mod model;
mod rulesets;
mod state;
mod util;
mod wizard;

use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("berthio {VERSION}");
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "berthio {VERSION}\n\n用法:\n  berthio                 启动面板（读取 BERTHIO_HOME，默认 127.0.0.1:18080）\n  berthio --demo          演示模式：临时状态、127.0.0.1:18090、内置演示数据\n  berthio --version       打印版本\n\n环境变量:\n  BERTHIO_HOME            数据目录（默认 ./.berthio，存在 /opt/berthio 时优先）\n  BERTHIO_LISTEN          覆盖监听地址\n  BERTHIO_DEMO_PORT       demo 监听端口（默认 18090）\n"
        );
        return;
    }
    if args.iter().any(|a| a == "--demo") {
        run_demo();
    } else {
        run_normal();
    }
}

fn run_normal() {
    let cfg = state::Config::resolve();
    let store = match state::Store::load(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[berthio] 状态加载失败: {}", e.message);
            std::process::exit(1);
        }
    };
    let listen = store.config.listen.clone();
    let store = Arc::new(store);
    http::serve(&listen, store, false);
}

fn run_demo() {
    let home = std::env::temp_dir().join(format!("berthio-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    state::init_home(&home);
    // 演示模式固定本机回环，不接受外部连接。
    std::env::set_var("BERTHIO_HOME", &home);
    let cfg = state::Config { home: home.clone() };
    let store = match state::Store::load(&cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[berthio] demo 状态初始化失败: {}", e.message);
            std::process::exit(1);
        }
    };
    if let Err(e) = store.seed_demo() {
        eprintln!("[berthio] demo 数据写入失败: {}", e.message);
        std::process::exit(1);
    }
    let port: u16 = std::env::var("BERTHIO_DEMO_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(18090);
    let listen = format!("127.0.0.1:{port}");
    println!("[berthio] demo 数据与状态目录: {}", home.display());
    println!("[berthio] demo 不启动真实内核、不写系统配置、不申请 root");
    http::serve(&listen, Arc::new(store), true);
}
