import Foundation

public enum GitHubLocalization {
    public static func text(_ key: String.LocalizationValue) -> String { String(localized: key, bundle: .module) }
}

func GHC(_ key: String.LocalizationValue) -> String { GitHubLocalization.text(key) }
