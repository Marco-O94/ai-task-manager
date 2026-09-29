// Renders the app icon's SVG source to a square PNG with transparency, through AppKit's own SVG
// support (no extra tools or crates). The PNG is the input of `cargo tauri icon`:
//
//   swift scripts/render-icon.swift src-tauri/icons/icon.svg target/icon-1024.png
//   cargo tauri icon target/icon-1024.png      # rewrites src-tauri/icons/* (drop android/ and ios/)
//
// usage: swift scripts/render-icon.swift <input.svg> <output.png> [size, default 1024]
import AppKit

let args = CommandLine.arguments
guard args.count == 3 || args.count == 4 else {
    FileHandle.standardError.write("usage: render-icon.swift <input.svg> <output.png> [size]\n".data(using: .utf8)!)
    exit(2)
}
let size = args.count == 4 ? Int(args[3]) ?? 0 : 1024
guard size > 0, let image = NSImage(contentsOf: URL(fileURLWithPath: args[1])) else {
    FileHandle.standardError.write("render-icon: cannot load \(args[1]) or bad size\n".data(using: .utf8)!)
    exit(1)
}
guard let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
    bytesPerRow: 0, bitsPerPixel: 0)
else { exit(1) }
rep.size = NSSize(width: size, height: size)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high
image.draw(in: NSRect(x: 0, y: 0, width: size, height: size), from: .zero, operation: .copy, fraction: 1)
NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
do {
    try png.write(to: URL(fileURLWithPath: args[2]))
} catch {
    FileHandle.standardError.write("render-icon: \(error)\n".data(using: .utf8)!)
    exit(1)
}
