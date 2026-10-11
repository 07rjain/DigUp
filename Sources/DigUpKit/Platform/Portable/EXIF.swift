// The two EXIF facts the indexer wants from a JPEG where ImageIO isn't there (Linux, Windows): the way the camera held
// the picture (orientation) and when it took it (DateTimeOriginal, as written). APP1 → TIFF header → IFD0 → Exif IFD.
#if !canImport(ImageIO)
import Foundation

struct EXIF {
    var orientation = 1
    var dateTimeOriginal: String?

    /// Reads the file's headers (the first 256 KB at most). Nil for anything but a JPEG with an Exif segment.
    static func read(_ url: URL) -> EXIF? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let data = try? handle.read(upToCount: 1 << 18), data.count > 4 else { return nil }
        let bytes = [UInt8](data)
        guard bytes[0] == 0xFF, bytes[1] == 0xD8 else { return nil }
        var i = 2
        while i + 4 <= bytes.count, bytes[i] == 0xFF {
            let marker = bytes[i + 1]
            if marker == 0xD8 || marker == 0x01 || (0xD0...0xD7).contains(marker) {   // markers without a length
                i += 2
                continue
            }
            if marker == 0xDA || marker == 0xD9 { break }   // the image data: no more headers
            let length = Int(bytes[i + 2]) << 8 | Int(bytes[i + 3])
            guard length >= 2 else { return nil }
            if marker == 0xE1, i + 10 <= bytes.count, bytes[(i + 4)..<(i + 10)].elementsEqual("Exif\0\0".utf8) {
                return parse(bytes, tiff: i + 10, end: min(bytes.count, i + 2 + length))
            }
            i += 2 + length
        }
        return nil
    }

    private static func parse(_ b: [UInt8], tiff: Int, end: Int) -> EXIF? {
        guard tiff + 8 <= end else { return nil }
        let little: Bool
        if b[tiff] == 0x49, b[tiff + 1] == 0x49 { little = true }
        else if b[tiff] == 0x4D, b[tiff + 1] == 0x4D { little = false }
        else { return nil }
        func u16(_ at: Int) -> Int? {
            guard at >= tiff, at + 2 <= end else { return nil }
            return little ? Int(b[at]) | Int(b[at + 1]) << 8 : Int(b[at]) << 8 | Int(b[at + 1])
        }
        func u32(_ at: Int) -> Int? {
            guard at >= tiff, at + 4 <= end else { return nil }
            return little ? Int(b[at]) | Int(b[at + 1]) << 8 | Int(b[at + 2]) << 16 | Int(b[at + 3]) << 24
                          : Int(b[at]) << 24 | Int(b[at + 1]) << 16 | Int(b[at + 2]) << 8 | Int(b[at + 3])
        }
        /// Calls `visit(tag, type, count, valueAt)` for each entry of the IFD at `at`.
        func scan(_ at: Int, _ visit: (Int, Int, Int, Int) -> Void) {
            guard let count = u16(at) else { return }
            for n in 0..<min(count, 500) {
                let entry = at + 2 + n * 12
                guard let tag = u16(entry), let type = u16(entry + 2), let number = u32(entry + 4) else { return }
                visit(tag, type, number, entry + 8)
            }
        }
        var exif = EXIF()
        var exifIFD: Int?
        guard let ifd0 = u32(tiff + 4) else { return nil }
        scan(tiff + ifd0) { tag, type, _, at in
            if tag == 0x0112, type == 3, let value = u16(at) { exif.orientation = value }
            if tag == 0x8769, type == 4, let offset = u32(at) { exifIFD = tiff + offset }
        }
        if let exifIFD {
            scan(exifIFD) { tag, type, count, at in
                guard tag == 0x9003, type == 2, count >= 19, let offset = u32(at), tiff + offset + 19 <= end
                else { return }
                exif.dateTimeOriginal = String(decoding: b[(tiff + offset)..<(tiff + offset + 19)], as: UTF8.self)
            }
        }
        return exif
    }
}
#endif
