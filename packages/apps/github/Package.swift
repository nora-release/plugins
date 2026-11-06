// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "NoraGitHub", defaultLocalization: "en", platforms: [.macOS(.v14)],
    products: [.executable(name: "PluginHost", targets: ["PluginHost"]), .executable(name: "PluginExtension", targets: ["PluginExtension"])],
    dependencies: [.package(url: "https://github.com/nora-release/plugin-kit.git", exact: "0.23.0"),
        .package(url: "https://github.com/Lakr233/MarkdownView.git", exact: "4.8.2"),
        .package(url: "https://github.com/scinfu/SwiftSoup.git", exact: "2.13.5"),
        .package(url: "https://github.com/fayazara/shiki-swift.git", exact: "0.1.0"),
    ],
    targets: [
        .target(name: "GitHubCore", resources: [.process("Resources")]),
        .target(name: "GitHubUI", dependencies: ["GitHubCore", .product(name: "MarkdownParser", package: "MarkdownView"), .product(name: "SwiftSoup", package: "SwiftSoup"), .product(name: "Shiki", package: "shiki-swift")], resources: [.copy("Resources/FileIcons"), .process("Resources/en.lproj"), .process("Resources/zh-Hans.lproj")]),
        .executableTarget(name: "PluginHost"),
        .executableTarget(name: "PluginExtension", dependencies: ["GitHubUI", "GitHubCore", .product(name: "NoraPluginKit", package: "plugin-kit")],
            swiftSettings: [.unsafeFlags(["-application-extension"])],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-application_extension", "-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])]),
        .testTarget(name: "GitHubCoreTests", dependencies: ["GitHubCore"]),
        .testTarget(name: "GitHubUITests", dependencies: ["GitHubUI", "GitHubCore"]),
    ], swiftLanguageModes: [.v6]
)
