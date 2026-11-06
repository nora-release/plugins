import Foundation

struct GitHubSourceDocument {
    let maximumColumns: Int
    init(_ source: String) {
        var columns = 0
        for unit in source.utf16 where unit != 13 {
            if unit == 9 { columns += 4 }
            else {
                let wide = (0x1100 ... 0x11FF).contains(unit) || (0x2E80 ... 0xA4CF).contains(unit)
                    || (0xAC00 ... 0xD7AF).contains(unit) || (0xF900 ... 0xFAFF).contains(unit)
                    || (0xFE10 ... 0xFE6F).contains(unit) || (0xFF00 ... 0xFF60).contains(unit)
                columns += wide ? 2 : 1
            }
        }
        maximumColumns = columns
    }
    static func language(fileName: String) -> String? {
        let name = (fileName as NSString).lastPathComponent.lowercased()
        if name == "dockerfile" || name.hasPrefix("dockerfile.") { return "dockerfile" }
        if name == "makefile" || name == "gnumakefile" { return "makefile" }
        if name == ".env" || name.hasPrefix(".env.") { return "dotenv" }
        let extensionName = (name as NSString).pathExtension
        let aliases = [
            "cc": "cpp", "cxx": "cpp", "hpp": "cpp", "h": "c", "cs": "csharp",
            "js": "javascript", "mjs": "javascript", "cjs": "javascript", "ts": "typescript",
            "py": "python", "rs": "rust", "rb": "ruby", "sh": "bash", "zsh": "bash",
            "m": "objective-c", "mm": "objective-cpp", "yml": "yaml", "plist": "xml",
            "conf": "ini", "cfg": "ini", "kt": "kotlin", "kts": "kotlin", "md": "markdown",
        ]
        guard !extensionName.isEmpty else { return nil }
        return aliases[extensionName] ?? extensionName
    }
}
