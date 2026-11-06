import Foundation

func GHL(_ key: String.LocalizationValue) -> String {
    String(localized: key, bundle: .module)
}
