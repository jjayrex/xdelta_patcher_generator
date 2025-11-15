fn main() {
    let mut res = winres::WindowsResource::new();
    res.set("FileDescription", "A tool to generate xdelta auto-patching executables.");
    res.set("ProductName", "xdelta Patch Generator");
    res.set("LegalCopyright", "JJayRex");
    res.set("FileVersion", "0.1.0.0");
    res.set("ProductVersion", "0.1.0.0");
    res.compile().unwrap();
}
