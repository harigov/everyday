// Composite PNGs onto one colour and write them back with no alpha channel.
//
//   swift scripts/flatten-png.swift '#161a4d' icon1.png icon2.png ...
//
// For the iOS app icon (`make icons`): iOS draws its own rounded corners
// over a square, and the App Store refuses an icon with an alpha channel.
// `cargo tauri icon --ios-color` is meant to do this and leaves the alpha in.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments.dropFirst()
guard let hex = args.first, hex.count == 7, let rgb = Int(hex.dropFirst(), radix: 16) else {
    FileHandle.standardError.write("usage: swift scripts/flatten-png.swift #rrggbb FILE...\n".data(using: .utf8)!); exit(2)
}
let (r, g, b) = (CGFloat((rgb >> 16) & 0xff) / 255, CGFloat((rgb >> 8) & 0xff) / 255, CGFloat(rgb & 0xff) / 255)
// Every file that could not be flattened is said, and makes the exit status
// 1: `make icons` copies these into the Xcode project, and an icon left with
// its alpha is one the App Store turns away.
var failed = 0
func fail(_ path: String, _ why: String) {
    FileHandle.standardError.write("flatten-png: \(path): \(why)\n".data(using: .utf8)!)
    failed += 1
}
for path in args.dropFirst() {
    let url = URL(fileURLWithPath: path)
    guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(src, 0, nil) else { fail(path, "not an image"); continue }
    let (w, h) = (image.width, image.height)
    guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                              space: CGColorSpace(name: CGColorSpace.sRGB)!,
                              bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { fail(path, "no context to draw in"); continue }
    ctx.setFillColor(red: r, green: g, blue: b, alpha: 1)
    ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
    ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
    guard let out = ctx.makeImage(),
          let dst = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else { fail(path, "could not be written"); continue }
    CGImageDestinationAddImage(dst, out, nil)
    if !CGImageDestinationFinalize(dst) { fail(path, "could not be written") }
}
exit(failed == 0 ? 0 : 1)
