// Draws Syndeo's application icon, every size an iconset holds, into the
// directory given (which must end in .iconset). `iconutil -c icns` turns that
// into ci/macos-app/Syndeo.icns, which is committed; this is how it was made.
//
//   swift ci/macos-app/make-icon.swift <dir>.iconset
//
// The mark: nodes joined to a centre, white on a blue-to-teal square with
// Apple's icon margins. "Syndeo" is Greek for binding together.
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

func draw(pixels: Int) -> CGImage {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0,
                        space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let s = CGFloat(pixels) / 1024.0
    ctx.scaleBy(x: s, y: s)

    // The rounded square: 824 points on a 1024 grid, corner radius 185.
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28,
                  color: CGColor(srgbRed: 0, green: 0, blue: 0, alpha: 0.30))
    ctx.addPath(shape)
    ctx.setFillColor(CGColor(srgbRed: 0.14, green: 0.39, blue: 0.92, alpha: 1))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(shape)
    ctx.clip()
    let gradient = CGGradient(colorsSpace: space, colors: [
        CGColor(srgbRed: 0.15, green: 0.39, blue: 0.92, alpha: 1),  // top
        CGColor(srgbRed: 0.05, green: 0.46, blue: 0.56, alpha: 1),  // bottom
    ] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    ctx.restoreGState()

    // Three nodes around a centre, each joined to it.
    let centre = CGPoint(x: 512, y: 500)
    let radius: CGFloat = 230
    let nodes = [90.0, 210.0, 330.0].map { (degrees: Double) -> CGPoint in
        let a = degrees * Double.pi / 180
        return CGPoint(x: centre.x + radius * CGFloat(cos(a)), y: centre.y + radius * CGFloat(sin(a)))
    }
    let white = CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1)
    ctx.setStrokeColor(white)
    ctx.setLineWidth(38)
    ctx.setLineCap(.round)
    for node in nodes {
        ctx.move(to: centre)
        ctx.addLine(to: node)
    }
    ctx.strokePath()
    ctx.setFillColor(white)
    for node in nodes {
        ctx.fillEllipse(in: CGRect(x: node.x - 72, y: node.y - 72, width: 144, height: 144))
    }
    ctx.fillEllipse(in: CGRect(x: centre.x - 96, y: centre.y - 96, width: 192, height: 192))
    ctx.setFillColor(CGColor(srgbRed: 0.10, green: 0.42, blue: 0.75, alpha: 1))
    ctx.fillEllipse(in: CGRect(x: centre.x - 46, y: centre.y - 46, width: 92, height: 92))
    return ctx.makeImage()!
}

let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
for (points, scale) in [(16, 1), (16, 2), (32, 1), (32, 2), (128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)] {
    let name = scale == 1 ? "icon_\(points)x\(points).png" : "icon_\(points)x\(points)@2x.png"
    let url = out.appendingPathComponent(name) as CFURL
    let dest = CGImageDestinationCreateWithURL(url, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, draw(pixels: points * scale), nil)
    guard CGImageDestinationFinalize(dest) else { fatalError("could not write \(name)") }
}
