// File kinds and the file system on Linux and Windows: by extension and POSIX calls, where macOS has Uniform Type
// Identifiers, the screenshot xattr and iCloud placeholders (Platform/Apple).
#if !canImport(UniformTypeIdentifiers)
import Foundation
#if canImport(Glibc)
import Glibc
#endif

extension Rules {
    /// What macOS gets from the type system, as extension sets. RAW pictures, which nothing here decodes yet, are still
    /// images: they fail with a note that says so, rather than vanish.
    static let imageExtensions: Set<String> = [
        "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "psd", "tga",
        "pnm", "ppm", "pgm", "cr2", "nef", "arw", "dng", "raf", "orf", "rw2",
    ]
    static let audioExtensions: Set<String> = [
        "mp3", "m4a", "m4b", "aac", "wav", "wave", "flac", "ogg", "oga", "opus", "aiff", "aif", "wma", "amr", "caf", "mka",
    ]
    static let videoExtensions: Set<String> = [
        "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "3gp", "3g2", "mpg", "mpeg", "m2v", "ogv", "mxf",
    ]

    /// `classify` as on macOS, by extension (for files with one, macOS's types come from the extension too).
    static func classify(_ url: URL, values: URLResourceValues?, options: IndexOptions) -> Classification {
        let ext = url.pathExtension.lowercased()
        if options.excludedExtensions.contains(ext) { return .skip("excluded type") }
        // .ts / .mts are usually TypeScript. Only a real transport stream is video.
        if ext == "ts" || ext == "mts" || ext == "m2ts" {
            if isMPEGTransportStream(url) { return .index(.video) }
            return options.includeCode ? .index(.doc) : .skip("code or data file")
        }
        if docExtensions.contains(ext) { return .index(.doc) }
        if codeExtensions.contains(ext) || sourceExtensions.contains(ext) {
            return options.includeCode ? .index(.doc) : .skip("code or data file")
        }
        if ext == "pdf" { return .index(.pdf) }
        if imageExtensions.contains(ext) { return .index(isScreenshot(url) ? .screenshot : .image) }
        if videoExtensions.contains(ext) { return .index(.video) }
        if audioExtensions.contains(ext) { return .index(.audio) }
        return .ignore
    }

    /// Only macOS marks screenshots; elsewhere their names tell (`screenshotPrefixes` covers Snipping Tool, Win+PrtScn,
    /// GNOME and KDE: they all start with "Screenshot").
    static func hasScreenshotMarker(_ url: URL) -> Bool { false }
}

enum FileSystem {
    /// What the crawl asks of each URL.
    static let crawlKeys: [URLResourceKey] = [.isDirectoryKey, .isPackageKey]

    /// Mac bundles that may sit on a shared or copied disk (an app, a Photos library): skipped whole here too, by name,
    /// since nothing else knows them as one thing.
    static let packageExtensions: Set<String> = [
        "app", "framework", "bundle", "plugin", "appex", "xpc", "kext", "xcodeproj", "xcworkspace", "playground",
        "photoslibrary", "musiclibrary", "tvlibrary", "imovielibrary", "fcpbundle", "logicx", "band", "pkg", "dmg",
    ]
    static func isPackage(_ url: URL, _ values: URLResourceValues?) -> Bool {
        url.hasDirectoryPath && packageExtensions.contains(url.pathExtension.lowercased())
    }

    /// No iCloud here. (OneDrive's Files On-Demand placeholders are a file attribute: `info`, on Windows.)
    static func isInCloud(_ values: URLResourceValues?) -> Bool { false }

    /// The file's own facts, without following a link.
    static func info(of path: String) -> FileInfo? {
        var info = stat()
        guard lstat(path, &info) == 0 else { return nil }
        return FileInfo(isRegular: (UInt32(info.st_mode) & UInt32(S_IFMT)) == UInt32(S_IFREG),
                        size: Int64(info.st_size),
                        mtime: Double(info.st_mtim.tv_sec) + Double(info.st_mtim.tv_nsec) / 1e9,
                        inode: UInt64(info.st_ino), device: UInt64(info.st_dev), dataless: false)
    }

    /// git on PATH; none → everything in a folder counts, as on a Mac without it.
    static let git: String? = executable("git")

    /// `name` on PATH.
    static func executable(_ name: String) -> String? {
        let path = ProcessInfo.processInfo.environment["PATH"] ?? "/usr/local/bin:/usr/bin:/bin"
        for folder in path.split(separator: ":") where !folder.isEmpty {
            let candidate = "\(folder)/\(name)"
            if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
        }
        return nil
    }
}
#endif
