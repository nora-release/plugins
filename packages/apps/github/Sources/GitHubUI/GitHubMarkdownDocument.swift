import Foundation
import MarkdownParser
import SwiftSoup

enum GitHubMarkdownAlignment: Equatable {
    case left, center, right
}

struct GitHubMarkdownTable: Equatable {
    let headers: [String]
    let rows: [[String]]
    let alignments: [GitHubMarkdownAlignment]
}

struct GitHubMarkdownTaskItem: Equatable {
    let isCompleted: Bool
    let children: [GitHubMarkdownBlock]
}

indirect enum GitHubMarkdownBlock: Equatable {
    case paragraph(String)
    case heading(level: Int, text: String)
    case code(language: String?, text: String)
    case image(source: String, alt: String)
    case quote([Self])
    case callout(kind: String, children: [Self])
    case list(orderedStart: Int?, items: [[Self]])
    case taskList(items: [GitHubMarkdownTaskItem])
    case table(GitHubMarkdownTable)
    case disclosure(title: String, children: [Self], isExpanded: Bool)
    case rule
}

enum GitHubMarkdownDocument {
    static func parse(_ source: String) -> [GitHubMarkdownBlock] {
        Parser().parse(source)
    }

    static func inline(_ source: String) -> AttributedString {
        let parser = Parser()
        let nodes = MarkdownParser().parse(source).document
        let markdown = nodes.map { node -> String in
            switch node {
            case let .paragraph(content), let .heading(_, content): parser.inline(content)
            default: source
            }
        }.joined(separator: "\n")
        var text = AttributedString()
        for (index, node) in MarkdownParser().parse(markdown).document.enumerated() {
            if index > 0 {
                text.append(AttributedString("\n"))
            }
            switch node {
            case let .paragraph(content), let .heading(_, content): text.append(attributed(content))
            default: text.append(AttributedString(markdown))
            }
        }
        return text
    }

    private static func attributed(_ nodes: [MarkdownInlineNode], intent: InlinePresentationIntent = []) -> AttributedString {
        var output = AttributedString()
        for node in nodes {
            let fragment: AttributedString
            switch node {
            case let .text(value), let .math(value, _):
                var text = AttributedString(value)
                if !intent.isEmpty {
                    text.inlinePresentationIntent = intent
                }
                fragment = text
            case .softBreak, .lineBreak: fragment = AttributedString("\n")
            case let .code(value):
                var text = AttributedString(value)
                text.inlinePresentationIntent = intent.union(.code)
                fragment = text
            case let .emphasis(children): fragment = attributed(children, intent: intent.union(.emphasized))
            case let .strong(children): fragment = attributed(children, intent: intent.union(.stronglyEmphasized))
            case let .strikethrough(children): fragment = attributed(children, intent: intent.union(.strikethrough))
            case let .link(destination, children):
                var label = attributed(children, intent: intent)
                if let url = URL(string: destination), ["https", "http", "mailto"].contains(url.scheme?.lowercased() ?? "") { label.link = url }
                fragment = label
            case let .image(_, children): fragment = attributed(children, intent: intent)
            case .html: fragment = AttributedString()
            }
            output.append(fragment)
        }
        return output
    }

    private final class Parser {
        private let markdown = MarkdownParser()
        private let maxDepth = 24
        private var literalHTMLCode: [String: [GitHubMarkdownBlock]] = [:]
        private var literalHTMLInlineCode: [String: String] = [:]
        private let inlineMarkerExpression = try? NSRegularExpression(pattern: #"NORAINLINE[A-F0-9]{32}N[0-9]+END"#)
        private let ignoredTags: Set<String> = ["script", "style", "iframe", "object", "embed", "noscript", "template"]
        private let containerTags: Set<String> = ["div", "section", "article", "main", "header", "footer", "aside", "p", "figure", "figcaption", "dl", "dt", "dd"]

        func parse(_ source: String, depth: Int = 0) -> [GitHubMarkdownBlock] {
            guard !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return [] }
            guard depth < maxDepth else { return [.paragraph(escape(source))] }
            let prepared = prepareHTML(source, depth: depth)
            return markdown.parse(prepared.source).document.flatMap { convert($0, replacements: prepared.blocks, depth: depth) }
        }

        private func convert(_ node: MarkdownBlockNode, replacements: [String: [GitHubMarkdownBlock]], depth: Int) -> [GitHubMarkdownBlock] {
            guard depth < maxDepth else { return [.paragraph(flatten(node))] }
            switch node {
            case let .paragraph(content):
                let text = inline(content)
                let key = text.trimmingCharacters(in: .whitespacesAndNewlines)
                if let replacement = replacements[key] ?? literalHTMLCode[key] {
                    return replacement
                }
                return paragraph(content)
            case let .heading(level, content): return [.heading(level: level, text: inline(content))]
            case let .codeBlock(language, content):
                return [.code(language: language, text: content.hasSuffix("\n") ? String(content.dropLast()) : content)]
            case let .blockquote(children):
                var blocks = children.flatMap { convert($0, replacements: replacements, depth: depth + 1) }
                if case let .paragraph(text)? = blocks.first, let callout = callout(text) {
                    if callout.text.isEmpty {
                        blocks.removeFirst()
                    } else {
                        blocks[0] = .paragraph(callout.text)
                    }
                    return [.callout(kind: callout.kind, children: blocks)]
                }
                return [.quote(blocks)]
            case let .bulletedList(_, items):
                return [.list(orderedStart: nil, items: items.map { $0.children.flatMap { convert($0, replacements: replacements, depth: depth + 1) } })]
            case let .numberedList(_, start, items):
                return [.list(orderedStart: start, items: items.map { $0.children.flatMap { convert($0, replacements: replacements, depth: depth + 1) } })]
            case let .taskList(_, items):
                return [.taskList(items: items.map { item in
                    GitHubMarkdownTaskItem(isCompleted: item.isCompleted, children: item.children.flatMap { convert($0, replacements: replacements, depth: depth + 1) })
                })]
            case let .table(alignments, rows):
                let cells = rows.map { $0.cells.map { inline($0.content) } }
                guard let headers = cells.first else { return [] }
                return [.table(.init(headers: headers, rows: Array(cells.dropFirst()), alignments: alignments.map {
                    switch $0 { case .center: .center; case .right: .right; default: .left }
                }))]
            case .thematicBreak: return [.rule]
            }
        }

        private func paragraph(_ nodes: [MarkdownInlineNode]) -> [GitHubMarkdownBlock] {
            var output: [GitHubMarkdownBlock] = []
            var prose: [MarkdownInlineNode] = []
            func flush() {
                let value = inline(prose)
                if !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { output.append(.paragraph(value)) }
                prose.removeAll()
            }
            for node in nodes {
                switch node {
                case let .image(source, children):
                    flush(); output.append(.image(source: source, alt: inline(children)))
                case let .link(_, children) where children.contains(where: { if case .image = $0 { return true }; return false }):
                    flush(); output += paragraph(children)
                case let .html(value) where htmlTag(value)?.name == "img":
                    flush()
                    if let image = (try? SwiftSoup.parseBodyFragment(value))?.body()?.getChildNodes().first as? Element {
                        output.append(.image(source: (try? image.attr("src")) ?? "", alt: (try? image.attr("alt")) ?? ""))
                    }
                default: prose.append(node)
                }
            }
            flush()
            return output
        }

        private func flatten(_ root: MarkdownBlockNode) -> String {
            var pending = [root]
            var values: [String] = []
            while let node = pending.popLast() {
                switch node {
                case let .paragraph(content), let .heading(_, content): values.append(inline(content))
                case let .codeBlock(_, content): values.append(codeSpan(content))
                case let .table(_, rows): values += rows.map { $0.cells.map { inline($0.content) }.joined(separator: " · ") }
                default: pending += node.children.reversed()
                }
            }
            return values.joined(separator: "\n")
        }

        func inline(_ nodes: [MarkdownInlineNode]) -> String {
            var output = ""
            var hiddenTag: String?
            var codeTag: String?
            var codeText = ""
            for node in nodes {
                if case let .html(value) = node {
                    if let tag = htmlTag(value), ignoredTags.contains(tag.name) {
                        if tag.isClosing, hiddenTag == tag.name {
                            hiddenTag = nil
                        } else if !tag.isClosing {
                            hiddenTag = tag.name
                        }
                        continue
                    }
                    if let tag = htmlTag(value), ["code", "kbd", "samp"].contains(tag.name) {
                        if tag.isClosing, codeTag == tag.name {
                            output += codeSpan(codeText)
                            codeText = ""
                            codeTag = nil
                        } else if !tag.isClosing {
                            codeTag = tag.name
                        }
                        continue
                    }
                    if codeTag != nil {
                        codeText += value; continue
                    }
                    if let tag = htmlTag(value), ["strong", "b", "em", "i", "s", "del", "strike"].contains(tag.name) {
                        if hiddenTag == nil {
                            output += ["strong", "b"].contains(tag.name) ? "**" : ["em", "i"].contains(tag.name) ? "*" : "~~"
                        }
                        continue
                    }
                    if htmlTag(value)?.name == "a" {
                        continue
                    }
                    if hiddenTag == nil {
                        output += htmlInline(value)
                    }
                    continue
                }
                guard hiddenTag == nil else { continue }
                if codeTag != nil {
                    switch node {
                    case let .text(value), let .code(value): codeText += value
                    case .softBreak, .lineBreak: codeText += "\n"
                    default: codeText += inline([node])
                    }
                    continue
                }
                switch node {
                case let .text(value): output += escape(value)
                case .softBreak, .lineBreak: output += "\n"
                case let .code(value): output += codeSpan(value)
                case let .emphasis(children): output += "*\(inline(children))*"
                case let .strong(children): output += "**\(inline(children))**"
                case let .strikethrough(children): output += "~~\(inline(children))~~"
                case let .link(destination, children):
                    output += "[\(inline(children))](\(destination))"
                case let .image(source, children):
                    let label = inline(children)
                    output += label.isEmpty ? escape(imageLabel(source: source)) : label
                case let .math(content, _): output += escape(content)
                case .html: break
                }
            }
            if codeTag != nil {
                output += codeSpan(codeText)
            }
            return output
        }

        private func callout(_ text: String) -> (kind: String, text: String)? {
            let unescaped = text.replacingOccurrences(of: "\\[", with: "[").replacingOccurrences(of: "\\]", with: "]")
            guard unescaped.hasPrefix("[!"), let end = unescaped.firstIndex(of: "]") else { return nil }
            let kind = String(unescaped[unescaped.index(unescaped.startIndex, offsetBy: 2) ..< end]).uppercased()
            guard ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"].contains(kind) else { return nil }
            let rest = String(unescaped[unescaped.index(after: end)...]).trimmingCharacters(in: .whitespacesAndNewlines)
            return (kind, rest)
        }

        private func escape(_ source: String) -> String {
            source.reduce(into: "") { result, character in
                if "\\`*_{}[]<>~".contains(character) {
                    result.append("\\")
                }
                result.append(character)
            }
        }

        private func codeSpan(_ text: String) -> String {
            let longest = text.split(whereSeparator: { $0 != "`" }).map(\.count).max() ?? 0
            let fence = String(repeating: "`", count: longest + 1)
            let padding = text.hasPrefix("`") || text.hasSuffix("`") || text.hasPrefix(" ") && text.hasSuffix(" ") ? " " : ""
            return "\(fence)\(padding)\(text)\(padding)\(fence)"
        }

        private func imageLabel(source: String, fallback: String = "") -> String {
            if !fallback.isEmpty {
                return fallback
            }
            guard let url = URL(string: source) else { return "—" }
            let name = url.deletingPathExtension().lastPathComponent
            if !name.isEmpty, name.count < 64, !name.allSatisfy(\.isNumber) {
                return name
            }
            return url.host ?? "—"
        }

        private struct PreparedHTML {
            let source: String
            let blocks: [String: [GitHubMarkdownBlock]]
        }

        private func prepareHTML(_ source: String, depth: Int) -> PreparedHTML {
            let ranges = markdown.parseBlockRange(source)
            let codeRanges = ranges.filter { $0.type == .codeBlock }.map { $0.startIndex ..< $0.endIndex }
                + codeSpans(source).map(\.range)
            var cursor = source.startIndex
            var result = ""
            var replacements: [String: [GitHubMarkdownBlock]] = [:]
            let nonce = "NORAHTML\(UUID().uuidString.replacingOccurrences(of: "-", with: ""))"
            for range in ranges where range.type == .htmlBlock {
                guard range.startIndex >= cursor else { continue }
                let end = htmlBlockEnd(source, range: range.startIndex ..< range.endIndex, codeRanges: codeRanges)
                guard end > range.startIndex else { continue }
                result += source[cursor ..< range.startIndex]
                let key = "\(nonce)\(replacements.count)"
                let fragment = String(source[range.startIndex ..< end])
                replacements[key] = htmlBlocks(fragment, depth: depth + 1)
                result += "\n\n\(key)\n\n"
                cursor = end
            }
            result += source[cursor...]
            return .init(source: result, blocks: replacements)
        }

        private struct HTMLTag {
            let name: String
            let isClosing: Bool
            let isSelfClosing: Bool
        }

        private func htmlTag(_ text: String) -> HTMLTag? {
            guard let expression = try? NSRegularExpression(pattern: #"^<\s*(/?)\s*([a-zA-Z][a-zA-Z0-9:-]*)"#),
                  let match = expression.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
                  let nameRange = Range(match.range(at: 2), in: text),
                  let closingRange = Range(match.range(at: 1), in: text) else { return nil }
            return .init(name: text[nameRange].lowercased(), isClosing: !text[closingRange].isEmpty, isSelfClosing: text.hasSuffix("/>"))
        }

        private func htmlBlockEnd(_ source: String, range: Range<String.Index>, codeRanges: [Range<String.Index>]) -> String.Index {
            let fragment = String(source[range])
            let trimmed = fragment.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let first = htmlTag(trimmed), ["details", "table", "div", "section", "article", "pre", "script", "style", "blockquote", "ul", "ol"].contains(first.name),
                  !first.isClosing, !first.isSelfClosing,
                  let expression = try? NSRegularExpression(pattern: #"<!--[\s\S]*?-->|<(?:(?:\"[^\"]*\")|(?:'[^']*')|[^'\">])*>"#) else { return range.upperBound }
            var nesting = 0
            for match in expression.matches(in: source, range: NSRange(range.lowerBound..., in: source)) {
                guard let tokenRange = Range(match.range, in: source),
                      !codeRanges.contains(where: { $0.contains(tokenRange.lowerBound) }),
                      let tag = htmlTag(String(source[tokenRange])), tag.name == first.name else { continue }
                if tag.isClosing {
                    nesting -= 1
                } else if !tag.isSelfClosing {
                    nesting += 1
                }
                if nesting == 0 {
                    return tokenRange.upperBound
                }
            }
            return first.name == "details" ? source.endIndex : range.upperBound
        }

        private func htmlBlocks(_ source: String, depth: Int) -> [GitHubMarkdownBlock] {
            var protectedSource = ""
            var cursor = source.startIndex
            let nonce = "NORACODE\(UUID().uuidString.replacingOccurrences(of: "-", with: ""))"
            for range in markdown.parseBlockRange(source) where range.type == .codeBlock {
                protectedSource += source[cursor ..< range.startIndex]
                let key = "\(nonce)\(literalHTMLCode.count)"
                let code = String(source[range.startIndex ..< range.endIndex])
                literalHTMLCode[key] = markdown.parse(code).document.flatMap { convert($0, replacements: [:], depth: depth) }
                protectedSource += "\n\n\(key)\n\n"
                cursor = range.endIndex
            }
            protectedSource += source[cursor...]
            protectedSource = protectInlineCode(protectedSource)
            guard let document = try? SwiftSoup.parseBodyFragment(protectedSource), let body = document.body() else { return [] }
            return htmlNodes(body.getChildNodes(), depth: depth)
        }

        private struct CodeSpan {
            let range: Range<String.Index>
            let text: String
        }

        private func codeSpans(_ source: String) -> [CodeSpan] {
            var spans: [CodeSpan] = []
            var cursor = source.startIndex
            while cursor < source.endIndex {
                if source[cursor] == "<", let end = tagEnd(source, start: cursor) {
                    cursor = end
                    continue
                }
                guard source[cursor] == "`" else { cursor = source.index(after: cursor); continue }
                var before = cursor
                var escapes = 0
                while before > source.startIndex {
                    before = source.index(before: before)
                    guard source[before] == "\\" else { break }
                    escapes += 1
                }
                let opening = cursor
                while cursor < source.endIndex, source[cursor] == "`" {
                    cursor = source.index(after: cursor)
                }
                guard escapes.isMultiple(of: 2) else { continue }
                let count = source.distance(from: opening, to: cursor)
                var closing = cursor
                while closing < source.endIndex {
                    guard let run = source[closing...].firstIndex(of: "`") else { break }
                    var end = run
                    while end < source.endIndex, source[end] == "`" {
                        end = source.index(after: end)
                    }
                    if source.distance(from: run, to: end) == count {
                        let candidate = String(source[opening ..< end])
                        if case let .paragraph(content)? = markdown.parse(candidate).document.first,
                           content.count == 1, case let .code(text) = content[0]
                        {
                            spans.append(.init(range: opening ..< end, text: codeSpan(text)))
                            cursor = end
                        }
                        break
                    }
                    closing = end
                }
            }
            return spans
        }

        private func tagEnd(_ source: String, start: String.Index) -> String.Index? {
            let next = source.index(after: start)
            guard next < source.endIndex, source[next].isLetter || "/!?".contains(source[next]) else { return nil }
            var cursor = next
            var quote: Character?
            while cursor < source.endIndex {
                let character = source[cursor]
                if let current = quote {
                    if character == current {
                        quote = nil
                    }
                } else if character == "\"" || character == "'" {
                    quote = character
                } else if character == ">" {
                    return source.index(after: cursor)
                }
                cursor = source.index(after: cursor)
            }
            return nil
        }

        private func protectInlineCode(_ source: String) -> String {
            let nonce = "NORAINLINE\(UUID().uuidString.replacingOccurrences(of: "-", with: ""))"
            var output = ""
            var cursor = source.startIndex
            for span in codeSpans(source) {
                output += source[cursor ..< span.range.lowerBound]
                let key = "\(nonce)N\(literalHTMLInlineCode.count)END"
                literalHTMLInlineCode[key] = span.text
                output += key
                cursor = span.range.upperBound
            }
            output += source[cursor...]
            return output
        }

        private func restoreInlineCode(_ source: String) -> String {
            guard source.contains("NORAINLINE"), let expression = inlineMarkerExpression else { return source }
            var output = ""
            var cursor = source.startIndex
            for match in expression.matches(in: source, range: NSRange(source.startIndex..., in: source)) {
                guard let range = Range(match.range, in: source) else { continue }
                output += source[cursor ..< range.lowerBound]
                output += literalHTMLInlineCode[String(source[range])] ?? String(source[range])
                cursor = range.upperBound
            }
            output += source[cursor...]
            return output
        }

        private func htmlNodes(_ nodes: [Node], depth: Int) -> [GitHubMarkdownBlock] {
            guard depth < maxDepth else {
                let text = nodes.map { plainHTML($0) }.joined()
                return text.isEmpty ? [] : [.paragraph(escape(text))]
            }
            var output: [GitHubMarkdownBlock] = []
            var prose = ""
            func flush() {
                output += parse(prose, depth: depth + 1)
                prose = ""
            }
            for node in nodes {
                if let text = node as? TextNode {
                    prose += restoreInlineCode(text.getWholeText()); continue
                }
                guard let element = node as? Element else { continue }
                let tag = element.tagNameNormal()
                guard !ignoredTags.contains(tag) else { continue }
                switch tag {
                case "details":
                    flush()
                    let summary = element.getChildNodes().compactMap { $0 as? Element }.first { $0.tagNameNormal() == "summary" }
                    let title = summary.map { htmlInlineNodes($0.getChildNodes()) }.flatMap { $0.isEmpty ? nil : $0 } ?? GHL("Details")
                    let children = element.getChildNodes().filter { $0 !== summary }
                    output.append(.disclosure(title: title, children: htmlNodes(children, depth: depth + 1), isExpanded: element.hasAttr("open")))
                case "table":
                    flush()
                    output += htmlTable(element, depth: depth)
                case "pre":
                    flush()
                    output.append(.code(language: nil, text: plainHTML(element)))
                case "blockquote":
                    flush()
                    output.append(.quote(htmlNodes(element.getChildNodes(), depth: depth + 1)))
                case "ul", "ol":
                    flush()
                    let items = element.getChildNodes().compactMap { $0 as? Element }.filter { $0.tagNameNormal() == "li" }
                    let start = tag == "ol" ? Int((try? element.attr("start")) ?? "") ?? 1 : nil
                    output.append(.list(orderedStart: start, items: items.map { htmlNodes($0.getChildNodes(), depth: depth + 1) }))
                case "img":
                    flush()
                    output.append(.image(source: (try? element.attr("src")) ?? "", alt: (try? element.attr("alt")) ?? ""))
                case "hr": flush(); output.append(.rule)
                case "h1", "h2", "h3", "h4", "h5", "h6":
                    flush()
                    output.append(.heading(level: Int(tag.dropFirst()) ?? 3, text: htmlInlineNodes(element.getChildNodes())))
                default:
                    if containerTags.contains(tag) {
                        flush()
                        output += htmlNodes(element.getChildNodes(), depth: depth + 1)
                    } else {
                        prose += htmlInlineNode(element)
                    }
                }
            }
            flush()
            return output
        }

        private func htmlTable(_ element: Element, depth: Int) -> [GitHubMarkdownBlock] {
            func rows(_ node: Node) -> [Element] {
                var output: [Element] = []
                var pending = [node]
                while let current = pending.popLast() {
                    guard let element = current as? Element else { continue }
                    if element.tagNameNormal() == "tr" {
                        output.append(element); continue
                    }
                    pending += element.getChildNodes().compactMap { $0 as? Element }.filter { $0.tagNameNormal() != "table" }.reversed()
                }
                return output
            }
            let rowElements = rows(element)
            let rowCells = rowElements.map { row in row.getChildNodes().compactMap { $0 as? Element }.filter { ["th", "td"].contains($0.tagNameNormal()) } }
            let cells = rowCells.map { $0.map { htmlInlineNodes($0.getChildNodes()).trimmingCharacters(in: .whitespacesAndNewlines) } }
            guard let first = cells.first else { return htmlNodes(element.getChildNodes(), depth: depth + 1) }
            guard cells.count <= 500, cells.allSatisfy({ $0.count <= 32 }) else {
                return [.paragraph(cells.map { $0.joined(separator: " · ") }.joined(separator: "\n"))]
            }
            let hasHeader = rowCells.first?.contains { $0.tagNameNormal() == "th" } == true
            let alignments: [GitHubMarkdownAlignment] = (rowCells.first ?? []).map { cell in
                let alignment = ((try? cell.attr("align")) ?? "").lowercased()
                let style = ((try? cell.attr("style")) ?? "").replacingOccurrences(of: " ", with: "").lowercased()
                if alignment == "center" || style.contains("text-align:center") {
                    return .center
                }
                if alignment == "right" || style.contains("text-align:right") {
                    return .right
                }
                return .left
            }
            return [.table(.init(headers: hasHeader ? first : [], rows: hasHeader ? Array(cells.dropFirst()) : cells, alignments: alignments))]
        }

        private func htmlInline(_ source: String) -> String {
            guard let body = (try? SwiftSoup.parseBodyFragment(source))?.body() else { return "" }
            return htmlInlineNodes(body.getChildNodes())
        }

        private func htmlInlineNodes(_ nodes: [Node], depth: Int = 0) -> String {
            guard depth < maxDepth else { return escape(nodes.map { plainHTML($0) }.joined()) }
            return nodes.map { htmlInlineNode($0, depth: depth + 1) }.joined()
        }

        private func htmlInlineNode(_ node: Node, depth: Int = 0) -> String {
            if let text = node as? TextNode {
                return restoreInlineCode(text.getWholeText())
            }
            guard let element = node as? Element else { return "" }
            let tag = element.tagNameNormal()
            guard !ignoredTags.contains(tag) else { return "" }
            guard depth < maxDepth else { return escape(plainHTML(element)) }
            let children = htmlInlineNodes(element.getChildNodes(), depth: depth + 1)
            switch tag {
            case "br": return "\n"
            case "strong", "b": return "**\(children)**"
            case "em", "i": return "*\(children)*"
            case "code", "kbd", "samp": return codeSpan(plainHTML(element))
            case "s", "del", "strike": return "~~\(children)~~"
            case "img":
                let alt = (try? element.attr("alt")) ?? ""
                let title = (try? element.attr("title")) ?? ""
                let source = (try? element.attr("src")) ?? ""
                let parentLink = element.parent()?.tagNameNormal() == "a" ? ((try? element.parent()?.attr("href")) ?? "") : ""
                return escape(imageLabel(source: source.isEmpty ? parentLink : source, fallback: alt.isEmpty ? title : alt))
            case "a":
                if !children.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    let href = (try? element.attr("href")) ?? ""
                    return "[\(children)](\(href))"
                }
                return escape(imageLabel(source: (try? element.attr("href")) ?? ""))
            default: return children
            }
        }

        private func plainHTML(_ node: Node) -> String {
            var output = ""
            var pending = [node]
            while let current = pending.popLast() {
                if let text = current as? TextNode {
                    output += restoreInlineCode(text.getWholeText())
                    continue
                }
                guard let element = current as? Element, !ignoredTags.contains(element.tagNameNormal()) else { continue }
                pending += element.getChildNodes().reversed()
            }
            return output
        }
    }
}
