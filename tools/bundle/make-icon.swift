// Render the Compme app icon to a 1024px PNG.
// Usage: swift make-icon.swift <output.png>
//
// Concept: inline text-completion. On a rounded-square blue→violet gradient,
// a solid white "typed" bar, a text caret, and a faded "ghost suggestion" bar
// — the product's core gesture (accept the ghost text after your cursor).
import AppKit

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write(Data("usage: swift make-icon.swift <output.png>\n".utf8))
    exit(2)
}
let outPath = CommandLine.arguments[1]
let size: CGFloat = 1024
let rect = NSRect(x: 0, y: 0, width: size, height: size)

let image = NSImage(size: rect.size)
image.lockFocus()

// Squircle: transparent corners so Finder shows a rounded icon.
let radius = size * 0.2237
NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).addClip()

// Diagonal gradient background.
let top = NSColor(srgbRed: 0.29, green: 0.33, blue: 0.95, alpha: 1) // indigo-blue
let bottom = NSColor(srgbRed: 0.55, green: 0.22, blue: 0.93, alpha: 1) // violet
NSGradient(colors: [top, bottom])!.draw(in: rect, angle: -60)

// Helper: a rounded white bar.
func bar(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, alpha: CGFloat) {
    NSColor(white: 1, alpha: alpha).setFill()
    NSBezierPath(roundedRect: NSRect(x: x, y: y, width: w, height: h),
                 xRadius: h / 2, yRadius: h / 2).fill()
}

let midY = size / 2
let barH: CGFloat = 104
// "typed" solid text (left of the caret).
bar(232, midY - barH / 2, 250, barH, alpha: 1.0)
// Text caret: a tall thin vertical bar.
let caretW: CGFloat = 60
NSColor(white: 1, alpha: 1).setFill()
NSBezierPath(roundedRect: NSRect(x: 512 - caretW / 2, y: midY - 190, width: caretW, height: 380),
             xRadius: caretW / 2, yRadius: caretW / 2).fill()
// "ghost suggestion" (right of the caret), faded.
bar(566, midY - barH / 2, 236, barH, alpha: 0.42)

image.unlockFocus()

guard let tiff = image.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff),
      let png = rep.representation(using: .png, properties: [:])
else {
    FileHandle.standardError.write(Data("failed to encode PNG\n".utf8))
    exit(1)
}
try! png.write(to: URL(fileURLWithPath: outPath))
