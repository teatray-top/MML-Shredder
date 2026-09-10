//! SVG에서 창 아이콘과 Windows 실행 파일 리소스를 생성한다.

use std::{env, error::Error, fs, io, path::PathBuf};

const ICON_SOURCE: &str = "assets/branding/app-icon.svg";
const WINDOW_ICON_SIZE: u32 = 256;
const ICON_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, WINDOW_ICON_SIZE];

/// SVG를 크기별로 렌더링하고 Windows 아이콘·버전 리소스를 연결한다.
fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed={ICON_SOURCE}");

    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join(ICON_SOURCE);
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let tree = resvg::usvg::Tree::from_data(&fs::read(source)?, &Default::default())?;
    let mut icons = ico::IconDir::new(ico::ResourceType::Icon);

    for size in ICON_SIZES {
        let rgba = render_icon(&tree, size)?;
        if size == WINDOW_ICON_SIZE {
            fs::write(output.join("app-icon.rgba"), &rgba)?;
        }
        let image = ico::IconImage::from_rgba_data(size, size, rgba);
        icons.add_entry(ico::IconDirEntry::encode(&image)?);
    }

    let icon_path = output.join("app-icon.ico");
    icons.write(fs::File::create(&icon_path)?)?;

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // 숫자 버전도 Cargo의 major/minor/patch를 사용한다.
        winresource::WindowsResource::new()
            .set_icon(icon_path.to_str().ok_or_else(|| {
                io::Error::other("The generated Windows icon path must be valid UTF-8")
            })?)
            .set("ProductName", "MML 세단기")
            .set("FileDescription", "MML 세단기")
            .set("FileVersion", &env::var("CARGO_PKG_VERSION")?)
            .set("ProductVersion", &env::var("CARGO_PKG_VERSION")?)
            .compile()?;
    }
    Ok(())
}

/// SVG를 지정 크기의 비곱셈 RGBA 픽셀로 변환한다.
fn render_icon(tree: &resvg::usvg::Tree, size: u32) -> Result<Vec<u8>, io::Error> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| io::Error::other("Could not allocate the application icon"))?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        size as f32 / tree.size().width(),
        size as f32 / tree.size().height(),
    );
    resvg::render(tree, transform, &mut pixmap.as_mut());

    // ICO와 egui의 반투명 경계는 비곱셈 RGBA가 필요하므로 alpha 곱셈을 해제한다.
    let mut rgba = Vec::with_capacity(size as usize * size as usize * 4);
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        rgba.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    Ok(rgba)
}
