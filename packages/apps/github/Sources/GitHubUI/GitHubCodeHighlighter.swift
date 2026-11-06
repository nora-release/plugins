import AppKit
import Shiki

@MainActor
public final class GitHubCodeHighlighter {
    private struct CacheKey: Hashable {
        let code: String
        let language: String

        static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.language == rhs.language && lhs.code.utf16.elementsEqual(rhs.code.utf16)
        }

        func hash(into hasher: inout Hasher) {
            hasher.combine(language)
            for unit in code.utf16 {
                hasher.combine(unit)
            }
        }
    }

    private struct CachedHighlight {
        let text: NSAttributedString
        let length: Int
    }

    private struct ColorPair: Hashable {
        let light: String
        let dark: String
    }

    private let font: NSFont
    private var engine: ShikiHighlighter?
    private var attemptedInitialization = false
    private var cache: [CacheKey: CachedHighlight] = [:]
    private var cacheOrder: [CacheKey] = []
    private var cachedLength = 0
    private var colors: [ColorPair: NSColor] = [:]
    private let maximumCodeLength = 24000
    private let maximumLineCount = 500
    private let maximumCacheLength = 192_000
    private let maximumCacheEntries = 32
    private let themes = [
        ShikiThemeVariant(colorName: "light", themeName: "vitesse-light"),
        ShikiThemeVariant(colorName: "dark", themeName: "vitesse-dark"),
    ]

    public init(fontSize: CGFloat = 13) {
        font = NSFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
    }

    public func attributedString(code: String, language: String?) -> NSAttributedString {
        let plain = NSAttributedString(string: code, attributes: [.font: font, .foregroundColor: NSColor.labelColor])
        guard !code.isEmpty, let name = normalizedLanguage(language),
              code.utf16.count <= maximumCodeLength,
              code.utf16.lazy.filter({ $0 == 10 }).prefix(maximumLineCount).count < maximumLineCount,
              let engine = highlighter(),
              let canonical = engine.assets.canonicalLanguageID(for: name)
        else { return plain }

        let key = CacheKey(code: code, language: canonical)
        if let cached = cache[key] {
            cacheOrder.removeAll { $0 == key }
            cacheOrder.append(key)
            return cached.text
        }

        guard let tokens = try? engine.codeToTokensWithThemes(
            code,
            language: canonical,
            themes: themes,
            options: .init(tokenizeMaxLineLength: 4096, tokenizeTimeLimit: 32),
        ) else { return plain }

        let styled = NSMutableAttributedString(attributedString: plain)
        let source = code as NSString
        for line in tokens {
            for token in line {
                let range = NSRange(location: token.offset, length: token.content.utf16.count)
                guard range.location >= 0, range.length <= source.length,
                      range.location <= source.length - range.length,
                      source.substring(with: range).utf16.elementsEqual(token.content.utf16)
                else { return plain }
                guard range.length > 0 else { continue }

                let light = token.variants["light"]
                let dark = token.variants["dark"]
                var attributes: [NSAttributedString.Key: Any] = [:]
                if let color = dynamicColor(light: light?.color, dark: dark?.color) {
                    attributes[.foregroundColor] = color
                }
                let style = light?.fontStyle ?? dark?.fontStyle ?? .none
                var tokenFont = font
                if style != .notSet {
                    if style.contains(.bold) {
                        tokenFont = NSFontManager.shared.convert(tokenFont, toHaveTrait: .boldFontMask)
                    }
                    if style.contains(.italic) {
                        tokenFont = NSFontManager.shared.convert(tokenFont, toHaveTrait: .italicFontMask)
                    }
                    if style.contains(.underline) {
                        attributes[.underlineStyle] = NSUnderlineStyle.single.rawValue
                    }
                    if style.contains(.strikethrough) {
                        attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
                    }
                }
                attributes[.font] = tokenFont
                styled.addAttributes(attributes, range: range)
            }
        }

        let result = NSAttributedString(attributedString: styled)
        cache[key] = CachedHighlight(text: result, length: source.length)
        cacheOrder.append(key)
        cachedLength += source.length
        trimCache()
        return result
    }

    private func normalizedLanguage(_ language: String?) -> String? {
        guard let language else { return nil }
        let name = language.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !name.isEmpty, !["text", "txt", "plain", "plaintext", "ansi"].contains(name) else { return nil }
        return name
    }

    private func highlighter() -> ShikiHighlighter? {
        if !attemptedInitialization {
            attemptedInitialization = true
            engine = try? ShikiHighlighter(defaultTheme: themes[0].themeName)
        }
        return engine
    }

    private func dynamicColor(light: String?, dark: String?) -> NSColor? {
        guard let light, let dark else { return nil }
        let pair = ColorPair(light: light, dark: dark)
        if let cached = colors[pair] {
            return cached
        }
        guard let lightColor = color(hex: light), let darkColor = color(hex: dark) else { return nil }
        let dynamic = NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? darkColor : lightColor
        }
        colors[pair] = dynamic
        return dynamic
    }

    private func color(hex: String) -> NSColor? {
        let digits = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard [6, 8].contains(digits.count), let value = UInt32(digits, radix: 16) else { return nil }
        let rgba = digits.count == 6 ? value << 8 | 0xFF : value
        return NSColor(
            srgbRed: CGFloat((rgba >> 24) & 0xFF) / 255,
            green: CGFloat((rgba >> 16) & 0xFF) / 255,
            blue: CGFloat((rgba >> 8) & 0xFF) / 255,
            alpha: CGFloat(rgba & 0xFF) / 255,
        )
    }

    private func trimCache() {
        while cache.count > maximumCacheEntries || cachedLength > maximumCacheLength {
            let key = cacheOrder.removeFirst()
            if let removed = cache.removeValue(forKey: key) {
                cachedLength -= removed.length
            }
        }
    }
}
