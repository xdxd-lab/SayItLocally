use std::path::{Path, PathBuf};

fn main() {
    // 必須在 tauri_build::build() 之前：tauri.windows.conf.json 裡把 transcribe-libs/*
    // 宣告成打包資源，而 tauri_build 在構建期就會校驗這個 glob 能不能匹配到檔案。
    // 目錄是這一步生成的，順序反了會報
    // "glob pattern transcribe-libs/* path not found or didn't match any files"。
    stage_transcribe_runtime_libs();
    link_clang_runtime_on_macos();

    tauri_build::build();
}

/// macOS：連結 clang 執行時庫 `libclang_rt.osx.a`。
///
/// transcribe-cpp 裡的 ggml-metal 用了 `@available(macOS 15.0, *)` 這類執行時版本判斷。
/// 部署目標低於 15（我們是 14.0）時，編譯器把它編成對 `___isPlatformVersionAtLeast` 的呼叫，
/// 這個符號在 clang 的執行時庫裡；rustc 連結時帶 `-nodefaultlibs`，不會自動鏈上它，
/// 結果就是 `Undefined symbols: ___isPlatformVersionAtLeast`。
/// 庫的位置向 clang 要（`--print-resource-dir`），Command Line Tools 和完整 Xcode 都適用。
fn link_clang_runtime_on_macos() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let resource_dir = std::process::Command::new("clang")
        .arg("--print-resource-dir")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let Some(resource_dir) = resource_dir else {
        println!("cargo:warning=沒找到 clang，無法連結 libclang_rt.osx.a（可能報 ___isPlatformVersionAtLeast 未定義）");
        return;
    };
    let lib_dir = Path::new(&resource_dir).join("lib").join("darwin");
    if !lib_dir.join("libclang_rt.osx.a").is_file() {
        println!("cargo:warning={} 裡沒有 libclang_rt.osx.a", lib_dir.display());
        return;
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=clang_rt.osx");
}

/// 把 transcribe-cpp 的執行時庫 + ggml 後端模組放到兩個地方：
///
/// 1. `target/<profile>/` —— 挨著 dev/release 執行檔，`cargo tauri dev` 和直接
///    跑 exe 時 `init_backends_default()` 才找得到模組。找不到的話註冊 0 個裝置，
///    載入 GGUF 模型直接報 TRANSCRIBE_ERR_BACKEND。
/// 2. `transcribe-libs/` —— 給 tauri 打包器當資源用（tauri.windows.conf.json 裡對映到 exe
///    同級目錄），這樣安裝出來的應用也帶著這些 DLL。
///
/// 靜態構建（沒開 dynamic-backends/shared）時 `DEP_TRANSCRIBE_CPP_*` 不存在，
/// 整個函式是 no-op。
fn stage_transcribe_runtime_libs() {
    println!("cargo:rerun-if-env-changed=DEP_TRANSCRIBE_CPP_RUNTIME_DIR");
    println!("cargo:rerun-if-env-changed=DEP_TRANSCRIBE_CPP_MODULE_DIR");

    let Some(runtime_dir) = std::env::var_os("DEP_TRANSCRIBE_CPP_RUNTIME_DIR") else {
        return; // 靜態連結，沒有要分發的東西
    };

    // RUNTIME_DIR = 核心庫（transcribe + ggml-base），MODULE_DIR = dlopen 的後端模組。
    // 兩個可能是同一個目錄，去重。
    let mut dirs = vec![PathBuf::from(runtime_dir)];
    if let Some(module_dir) = std::env::var_os("DEP_TRANSCRIBE_CPP_MODULE_DIR") {
        let p = PathBuf::from(module_dir);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }

    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out → 上溯三層 = target/<profile>
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR 形狀變了")
        .to_path_buf();

    let staging = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("transcribe-libs");
    // 重建 staging，避免上次構建遺留的、這次已經改名/刪掉的模組被一起打包。
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).expect("建立 transcribe-libs 目錄");

    let mut copied = 0usize;
    for dir in &dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let src = entry.path();
            let name = match src.file_name().and_then(|s| s.to_str()) {
                Some(n) if n.ends_with(".dll") => n.to_string(),
                _ => continue,
            };
            copy_to(&src, &profile_dir, &name);
            copy_to(&src, &staging, &name);
            copied += 1;
        }
    }

    if copied == 0 {
        panic!(
            "transcribe-cpp 是 shared/dynamic-backends 構建，但在 {dirs:?} 裡沒找到任何 DLL；\
             這樣打出來的包會註冊 0 個計算裝置、本地 GGUF 模型載入必失敗"
        );
    }
    println!("cargo:warning=staged {copied} transcribe/ggml DLL(s)");

    // 上面這些 DLL 是 C++ 編的，載入期就要 VC++ 執行庫，必須一起分發。見下面註釋。
    let crt = stage_vc_redist(&profile_dir, &staging);
    println!("cargo:warning=staged {crt} VC++ runtime DLL(s)");
}

fn copy_to(src: &Path, dir: &Path, name: &str) {
    if let Err(e) = std::fs::copy(src, dir.join(name)) {
        // 目標被佔用（應用正在執行）時不要炸構建，只提示。
        println!("cargo:warning=copy {name} -> {} failed: {e}", dir.display());
    }
}

/// 把 VC++ 執行庫（MSVCP140 / VCRUNTIME140 / VCRUNTIME140_1 …）拷到 exe 同級，
/// 跟著安裝包一起分發。返回拷了幾個。
///
/// 為什麼非做不可（0.1.4 真砸過一台機器）：
/// 上面那些 transcribe/ggml DLL 是 C++ 編的，**載入期**就依賴 MSVCP140.dll。
/// 微軟的規則是「機器上的執行庫版本不得低於編譯時的 MSVC 工具集版本」，
/// 反方向不保證相容。我們用 14.44 編，某台 Win10 上是 2019 年的 14.24：
/// DLL 能載入，但第一次 std::mutex::lock() 直接訪問違例（0xc0000005）。
/// 崩點在 init_backends()，那時視窗還沒建立、日誌剛寫兩行 ——
/// 使用者看到的就是「雙擊沒任何反應，也沒有任何報錯」。
///
/// 為什麼用 app-local 而不是讓安裝包去裝 vc_redist：
/// exe 所在目錄在 DLL 搜尋順序裡排在 System32 前面，同目錄放一份新的就會優先命中；
/// 而且我們是 currentUser 免持權安裝，本來就沒法跑需要管理員的 vc_redist。
/// 這也是 Handy 的做法（它在 CI 裡算好目錄塞給 build.rs，我們直接在這兒自己找）。
fn stage_vc_redist(profile_dir: &Path, staging: &Path) -> usize {
    println!("cargo:rerun-if-env-changed=SAYIT_VC_REDIST_DIRS");
    println!("cargo:rerun-if-env-changed=SAYIT_SKIP_VC_REDIST");
    println!("cargo:rerun-if-env-changed=VCToolsRedistDir");

    // 只有 MSVC 目標才有這套執行庫
    if std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() != "msvc" {
        return 0;
    }
    if std::env::var_os("SAYIT_SKIP_VC_REDIST").is_some() {
        println!(
            "cargo:warning=SAYIT_SKIP_VC_REDIST 已設定：不隨附 VC++ 執行庫。\
             這樣打出來的包在執行庫比構建工具集舊的機器上會啟動即崩，別拿去發版。"
        );
        return 0;
    }

    let dirs = resolve_vc_redist_dirs().unwrap_or_else(|| {
        panic!(
            "找不到 VC++ 執行庫 redist 目錄，無法把 MSVCP140/VCRUNTIME140 隨應用分發。\n\
             不帶這幾個 DLL 打出來的包，在執行庫版本低於構建工具集的機器上會在 \
             init_backends() 裡訪問違例崩掉（0xc0000005，無視窗、無報錯、日誌只有開頭兩行）。\n\
             解決其一：\n\
               1. VS Installer 裡裝上「MSVC v143 - VS 2022 C++ x64/x86 生成工具（最新）」，\
                  它帶 VC\\Redist\\MSVC\\<ver>\\x64\\Microsoft.VC143.CRT；\n\
               2. 手動指定 SAYIT_VC_REDIST_DIRS=<CRT 目錄>[;<OpenMP 目錄>]；\n\
               3. 明確要打不帶執行庫的包（只在本機自測）：SAYIT_SKIP_VC_REDIST=1。"
        )
    });

    let mut copied = 0usize;
    for dir in &dirs {
        println!("cargo:rerun-if-changed={}", dir.display());
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let src = entry.path();
            let name = match src.file_name().and_then(|s| s.to_str()) {
                Some(n) if n.to_ascii_lowercase().ends_with(".dll") => n.to_string(),
                _ => continue,
            };
            copy_to(&src, profile_dir, &name);
            copy_to(&src, staging, &name);
            copied += 1;
        }
    }

    if copied == 0 {
        panic!("VC++ redist 目錄 {dirs:?} 裡一個 DLL 都沒有，裝的 VS 元件可能不完整");
    }
    copied
}

/// 定位要隨附的執行庫目錄。優先順序：顯式指定 → VS 開發環境變數 → vswhere 現查。
///
/// 關鍵是**取和編譯這些 DLL 的同一套 VS 的 redist**，不能隨便拿個新版本或
/// System32 裡的湊數：System32 裡那份正是使用者機器上可能過舊的東西，
/// 而版本必須 >= 構建工具集。
fn resolve_vc_redist_dirs() -> Option<Vec<PathBuf>> {
    // 1. 顯式指定（分號分隔），給 CI / 特殊構建環境留的口子
    if let Some(v) = std::env::var_os("SAYIT_VC_REDIST_DIRS") {
        let list: Vec<PathBuf> = v
            .to_string_lossy()
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect();
        if !list.is_empty() {
            return Some(list);
        }
    }

    let redist_root = vc_redist_root()?;
    // 我們只出 x64，但別把 arch 寫死，將來上 ARM64 不用再回來改。
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH")
        .unwrap_or_default()
        .as_str()
    {
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    };
    let arch_dir = redist_root.join(arch);

    // 目錄名帶 VC 代次（Microsoft.VC143.CRT），按名字排序取最新的，
    // 免得工具鏈升代（VC144…）後這裡失配。
    let crt = newest_redist_subdir(&arch_dir, ".crt")?;
    let mut dirs = vec![crt];
    // OpenMP（vcomp140.dll）：當前構建的 ggml 沒開 OpenMP，import 表裡也沒有它。
    // 但 transcribe-cpp-sys 在 Windows 上是「傾向開 OpenMP」的，將來升版本一旦
    // 翻回 ON，缺 vcomp140 又是一次啟動即崩。有就帶上，多 200KB 買個保險。
    if let Some(omp) = newest_redist_subdir(&arch_dir, ".openmp") {
        dirs.push(omp);
    }
    Some(dirs)
}

/// `VC\Redist\MSVC\<version>` 的絕對路徑。
fn vc_redist_root() -> Option<PathBuf> {
    // VS 開發者命令列裡會設這個，直接指向 VC\Redist\MSVC\<ver>\
    if let Some(v) = std::env::var_os("VCToolsRedistDir") {
        let p = PathBuf::from(v);
        if p.is_dir() {
            return Some(p);
        }
    }

    // 普通終端裡沒有上面那個變數，用 vswhere 現查。VS 把「該配哪個 redist 版本」
    // 寫在 Microsoft.VCRedistVersion.default.txt 裡，照它取才和編譯器對得上。
    let program_files_x86 = std::env::var("ProgramFiles(x86)")
        .or_else(|_| std::env::var("ProgramFiles"))
        .ok()?;
    let vswhere = PathBuf::from(program_files_x86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }

    let out = std::process::Command::new(&vswhere)
        .args(["-latest", "-products", "*", "-property", "installationPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let vs_root = PathBuf::from(
        stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())?,
    );

    let version = std::fs::read_to_string(
        vs_root
            .join("VC")
            .join("Auxiliary")
            .join("Build")
            .join("Microsoft.VCRedistVersion.default.txt"),
    )
    .ok()?;
    let root = vs_root
        .join("VC")
        .join("Redist")
        .join("MSVC")
        .join(version.trim());
    if root.is_dir() {
        Some(root)
    } else {
        None
    }
}

/// 在 `<redist>\<arch>\` 下找形如 `Microsoft.VC143.CRT` 的子目錄，按名字取最新的。
/// `suffix` 傳小寫（".crt" / ".openmp"），比較時忽略大小寫。
fn newest_redist_subdir(arch_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = std::fs::read_dir(arch_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| {
                    let n = n.to_ascii_lowercase();
                    n.starts_with("microsoft.vc") && n.ends_with(suffix)
                })
                .unwrap_or(false)
        })
        .collect();
    hits.sort();
    hits.pop()
}
