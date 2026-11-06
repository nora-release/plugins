import Foundation

struct GitHubDiffDocument: Sendable {
    enum LineKind: Sendable { case context, addition, deletion, annotation }

    struct Line: Sendable {
        let kind: LineKind
        let content: String
        let oldNumber: Int?
        let newNumber: Int?
    }

    struct Hunk: Sendable {
        let header: String
        var lines: [Line] = []
    }

    struct File: Sendable {
        var oldPath = ""
        var newPath = ""
        var hunks: [Hunk] = []
        var metadata: [String] = []

        var title: String {
            if oldPath.isEmpty || oldPath == "/dev/null" { return newPath }
            if newPath.isEmpty || newPath == "/dev/null" || newPath == oldPath { return oldPath }
            return oldPath + " → " + newPath
        }

        var additions: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .addition }.count } }
        var deletions: Int { hunks.reduce(0) { $0 + $1.lines.filter { $0.kind == .deletion }.count } }
    }

    let files: [File]
    var maximumColumns = 0

    static func parse(_ patch: String) -> Self {
        let expression = try! NSRegularExpression(pattern: #"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@"#)
        var files: [File] = []
        var file: File?
        var oldNumber = 0
        var newNumber = 0
        var remainingOld = 0
        var remainingNew = 0
        var inHunk = false
        for raw in patch.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 == "\r\n" }) {
            if Task.isCancelled { return Self(files: []) }
            let line = raw.hasSuffix("\r") ? String(raw.dropLast()) : String(raw)
            if line.hasPrefix("diff --git ") {
                if let file { files.append(file) }
                let paths = gitPaths(String(line.dropFirst(11)))
                file = File(oldPath: paths.first.map(normalizedPath) ?? "", newPath: paths.last.map(normalizedPath) ?? "")
                inHunk = false
                continue
            }
            if let match = expression.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)) {
                let source = line as NSString
                let oldCount = match.range(at: 2).location == NSNotFound ? 1 : Int(source.substring(with: match.range(at: 2)))
                let newCount = match.range(at: 4).location == NSNotFound ? 1 : Int(source.substring(with: match.range(at: 4)))
                guard let oldStart = Int(source.substring(with: match.range(at: 1))),
                      let newStart = Int(source.substring(with: match.range(at: 3))),
                      let oldCount, let newCount,
                      !oldStart.addingReportingOverflow(oldCount).overflow,
                      !newStart.addingReportingOverflow(newCount).overflow else {
                    return Self(files: [])
                }
                if file == nil { file = File() }
                oldNumber = oldStart
                newNumber = newStart
                remainingOld = oldCount
                remainingNew = newCount
                file?.hunks.append(Hunk(header: line))
                inHunk = true
                continue
            }
            if inHunk, let index = file?.hunks.indices.last {
                if line.hasPrefix("\\ No newline at end of file") {
                    file?.hunks[index].lines.append(Line(kind: .annotation, content: line, oldNumber: nil, newNumber: nil))
                    continue
                }
                if remainingOld > 0 || remainingNew > 0 {
                    if line.hasPrefix("-") {
                        guard oldNumber < Int.max else { return Self(files: []) }
                        file?.hunks[index].lines.append(Line(kind: .deletion, content: String(line.dropFirst()), oldNumber: oldNumber, newNumber: nil))
                        oldNumber += 1
                        remainingOld -= 1
                    } else if line.hasPrefix("+") {
                        guard newNumber < Int.max else { return Self(files: []) }
                        file?.hunks[index].lines.append(Line(kind: .addition, content: String(line.dropFirst()), oldNumber: nil, newNumber: newNumber))
                        newNumber += 1
                        remainingNew -= 1
                    } else if line.hasPrefix(" ") {
                        guard oldNumber < Int.max, newNumber < Int.max else { return Self(files: []) }
                        file?.hunks[index].lines.append(Line(kind: .context, content: String(line.dropFirst()), oldNumber: oldNumber, newNumber: newNumber))
                        oldNumber += 1
                        newNumber += 1
                        remainingOld -= 1
                        remainingNew -= 1
                    }
                    continue
                }
                inHunk = false
            }
            if line.hasPrefix("--- ") {
                if let previous = file, !previous.hunks.isEmpty { files.append(previous); file = nil }
                if file == nil { file = File() }
                file?.oldPath = normalizedPath(String(line.dropFirst(4)).components(separatedBy: "\t")[0])
            } else if line.hasPrefix("+++ ") {
                file?.newPath = normalizedPath(String(line.dropFirst(4)).components(separatedBy: "\t")[0])
            } else if line.hasPrefix("rename from ") {
                file?.oldPath = decodedPath(String(line.dropFirst(12)))
            } else if line.hasPrefix("rename to ") {
                file?.newPath = decodedPath(String(line.dropFirst(10)))
            } else if line.hasPrefix("Binary files ") || line == "GIT binary patch" {
                file?.metadata.append(GHL("Binary files differ"))
            } else if line.hasPrefix("old mode ") || line.hasPrefix("new mode ") || line.hasPrefix("new file mode ") || line.hasPrefix("deleted file mode ") {
                file?.metadata.append(line)
            }
        }
        if let file { files.append(file) }
        let visible = files.filter { !$0.oldPath.isEmpty || !$0.newPath.isEmpty || !$0.hunks.isEmpty }
        let columns = visible.lazy.flatMap(\.hunks).flatMap(\.lines).map { GitHubSourceDocument($0.content).maximumColumns }.max() ?? 0
        return Self(files: visible, maximumColumns: columns)
    }

    private static func normalizedPath(_ path: String) -> String {
        let decoded = decodedPath(path)
        if decoded.hasPrefix("a/") || decoded.hasPrefix("b/") { return String(decoded.dropFirst(2)) }
        return decoded
    }

    private static func gitPaths(_ value: String) -> [String] {
        if !value.contains("\""), value.hasPrefix("a/") {
            var candidate = value.startIndex
            var boundaries: [Range<String.Index>] = []
            while let boundary = value.range(of: " b/", range: candidate ..< value.endIndex) {
                boundaries.append(boundary)
                candidate = boundary.upperBound
            }
            let matching = boundaries.first { boundary in
                value[value.index(value.startIndex, offsetBy: 2) ..< boundary.lowerBound]
                    == value[boundary.upperBound ..< value.endIndex]
            }
            if let boundary = matching ?? boundaries.last {
                return [String(value[..<boundary.lowerBound]), String(value[value.index(after: boundary.lowerBound)...])]
            }
        }
        var paths: [String] = []
        var current = ""
        var quoted = false
        var escaped = false
        for character in value {
            if escaped { current.append(character); escaped = false; continue }
            if character == "\\", quoted { current.append(character); escaped = true; continue }
            if character == "\"" { quoted.toggle(); current.append(character); continue }
            if character == " ", !quoted {
                if !current.isEmpty { paths.append(current); current = "" }
            } else { current.append(character) }
        }
        if !current.isEmpty { paths.append(current) }
        return paths
    }

    private static func decodedPath(_ path: String) -> String {
        guard path.hasPrefix("\""), path.hasSuffix("\"") else { return path }
        let characters = Array(path.dropFirst().dropLast())
        var bytes: [UInt8] = []
        var index = 0
        while index < characters.count {
            if characters[index] == "\\", index + 1 < characters.count {
                index += 1
                if ("0" ... "7").contains(String(characters[index])) {
                    var octal = ""
                    while index < characters.count, octal.count < 3, ("0" ... "7").contains(String(characters[index])) {
                        octal.append(characters[index]); index += 1
                    }
                    if let byte = UInt8(octal, radix: 8) { bytes.append(byte) }
                    continue
                }
                let escaped = [Character("t"): "\t", "n": "\n", "r": "\r"]
                bytes.append(contentsOf: (escaped[characters[index]] ?? String(characters[index])).utf8)
            } else { bytes.append(contentsOf: String(characters[index]).utf8) }
            index += 1
        }
        return String(decoding: bytes, as: UTF8.self)
    }
}

struct GitHubDiffDisplayRow: Sendable {
    enum Kind: Sendable { case file, hunk, line, annotation }
    let kind: Kind
    let fileIndex: Int
    var title = ""
    var left: GitHubDiffDocument.Line?
    var right: GitHubDiffDocument.Line?
    var leftChange: NSRange?
    var rightChange: NSRange?

    static func rows(document: GitHubDiffDocument, split: Bool, collapsedFiles: Set<Int>, showsFileHeaders: Bool) -> [Self] {
        var rows: [Self] = []
        for (index, file) in document.files.enumerated() {
            if Task.isCancelled { return [] }
            if showsFileHeaders { rows.append(Self(kind: .file, fileIndex: index, title: file.title)) }
            if showsFileHeaders, collapsedFiles.contains(index) { continue }
            for metadata in file.metadata { rows.append(Self(kind: .annotation, fileIndex: index, title: metadata)) }
            for hunk in file.hunks {
                rows.append(Self(kind: .hunk, fileIndex: index, title: hunk.header))
                var lineIndex = 0
                while lineIndex < hunk.lines.count {
                    if Task.isCancelled { return [] }
                    let line = hunk.lines[lineIndex]
                    if line.kind == .annotation {
                        rows.append(Self(kind: .annotation, fileIndex: index, title: line.content))
                        lineIndex += 1
                    } else if line.kind == .context {
                        rows.append(Self(kind: .line, fileIndex: index, left: line, right: split ? line : nil))
                        lineIndex += 1
                    } else {
                        var deletions: [GitHubDiffDocument.Line] = []
                        var additions: [GitHubDiffDocument.Line] = []
                        while lineIndex < hunk.lines.count, [.deletion, .addition].contains(hunk.lines[lineIndex].kind) {
                            let changed = hunk.lines[lineIndex]
                            if changed.kind == .deletion { deletions.append(changed) }
                            else { additions.append(changed) }
                            lineIndex += 1
                        }
                        if split { for offset in 0 ..< max(deletions.count, additions.count) {
                            let left = deletions.indices.contains(offset) ? deletions[offset] : nil
                            let right = additions.indices.contains(offset) ? additions[offset] : nil
                            let ranges = changedRanges(left?.content, right?.content)
                            rows.append(Self(kind: .line, fileIndex: index, left: left, right: right, leftChange: ranges.0, rightChange: ranges.1))
                        } }
                        if !split {
                            for (offset, left) in deletions.enumerated() {
                                let right = additions.indices.contains(offset) ? additions[offset] : nil
                                rows.append(Self(kind: .line, fileIndex: index, left: left, leftChange: changedRanges(left.content, right?.content).0))
                            }
                            for (offset, right) in additions.enumerated() {
                                let left = deletions.indices.contains(offset) ? deletions[offset] : nil
                                rows.append(Self(kind: .line, fileIndex: index, left: right, leftChange: changedRanges(left?.content, right.content).1))
                            }
                        }
                    }
                }
            }
        }
        return rows
    }

    private static func changedRanges(_ left: String?, _ right: String?) -> (NSRange?, NSRange?) {
        guard let left, let right, left.utf16.count <= 4096, right.utf16.count <= 4096 else { return (nil, nil) }
        let old = Array(left), new = Array(right)
        var prefix = 0
        while prefix < min(old.count, new.count), old[prefix] == new[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < min(old.count, new.count) - prefix, old[old.count - suffix - 1] == new[new.count - suffix - 1] { suffix += 1 }
        func range(_ characters: [Character]) -> NSRange {
            let location = String(characters.prefix(prefix)).utf16.count
            let length = String(characters[prefix ..< characters.count - suffix]).utf16.count
            return NSRange(location: location, length: length)
        }
        return (range(old), range(new))
    }
}

extension GitHubDiffDocument {
    func patch(fileName: String) -> String? {
        guard let file = files.first(where: { $0.newPath == fileName || $0.oldPath == fileName }), !file.hunks.isEmpty else { return nil }
        return file.hunks.map { hunk in
            ([hunk.header] + hunk.lines.map { line in
                switch line.kind {
                case .context: " " + line.content
                case .addition: "+" + line.content
                case .deletion: "-" + line.content
                case .annotation: line.content
                }
            }).joined(separator: "\n")
        }.joined(separator: "\n")
    }
}
