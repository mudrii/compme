// Render the Compme placeholder app icon to a 1024px PNG.
// Usage: swift make-icon.swift <output.png>
// A rounded-square (squircle) indigo→violet gradient with a centered white "C".
import AppKit

let outPath = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon-1024.png"
let size: CGFloat = 1024
let rect = NSRect(x: 0, y: 0, width: size, height: size)

let image = NSImage(size: rect.size)
image.lockFocus()

// Transparent corners: clip to a rounded rect so Finder shows a squircle.
let radius = size * 0.2237 // macOS continuous-corner ratio, approximated
NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).addClip()

// Vertical gradient background.
let top = NSColor(srgbRed: 0.31, green: 0.27, blue: 0.90, alpha: 1) // indigo
let bottom = NSColor(srgbRed: 0.49, green: 0.23, blue: 0.93, alpha: 1) // violet
NSGradient(colors: [top, bottom])!.draw(in: rect, angle: -90)

// Centered white "C".
let para = NSMutableParagraphStyle()
para.alignment = .center
let font = NSFont.systemFont(ofSize: 640, weight: .bold)
let attrs: [NSAttributedString.Key: Any] = [
    .font: font,
    .foregroundColor: NSColor.white,
    .paragraphStyle: para,
]
let glyph = "C" as NSString
let g = glyph.size(withAttributes: attrs)
glyph.draw(at: NSPoint(x: (size - g.width) / 2, y: (size - g.height) / 2), withAttributes: attrs)

image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:])
else {
    FileHandle.standardError.write(Data("failed to encode PNG\n".utf8))
    exit(1)
}
try! png.write(to: URL(fileURLWithPath: outPath))
