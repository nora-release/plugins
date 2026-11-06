import AppKit
import GitHubCore
import SwiftUI

struct GitHubFilesView: View {
    @Bindable var service: GitHubService
    @State private var selectedPath: String?
    @State private var layout = GitHubDiffViewController.Layout.unified
    @State private var contentHeight: CGFloat = 0
    @State private var fullPatch: String?
    @State private var loadingPatch = false
    @State private var patchError: String?
    @State private var patchRequest = UUID()
    private var files: [GitHubValue] { service.sections[.files]?.items ?? [] }
    private var selected: GitHubValue? { files.first { $0["filename"].text == selectedPath } ?? files.first }

    var body: some View {
        HSplitView {
            ScrollView {
                LazyVStack(spacing: 3) {
                    ForEach(files, id: \.self) { file in
                        Button { selectedPath = file["filename"].text } label: {
                            HStack(spacing: 7) {
                                Image(nsImage: GitHubFileIcon.image(fileName: file["filename"].text)).resizable().scaledToFit().frame(width: 17, height: 17)
                                Text(file["filename"].text).font(.system(size: 12, weight: .medium)).lineLimit(1).truncationMode(.middle)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                GitHubChangeCount(item: file)
                            }
                            .padding(.horizontal, 9).frame(height: 34)
                            .background(selected?["filename"] == file["filename"] ? Color.primary.opacity(0.08) : .clear, in: RoundedRectangle(cornerRadius: 5))
                            .contentShape(RoundedRectangle(cornerRadius: 5))
                        }.buttonStyle(.plain).help(file["filename"].text)
                    }
                    if service.sections[.files]?.loading == true { GitHubSkeleton(rows: 5, sidebar: true) }
                    GitHubSectionError(service: service, section: .files)
                }.padding(8)
            }.frame(minWidth: 160, idealWidth: 220, maxWidth: 300)
            GeometryReader { geometry in
                if let file = selected {
                    VStack(spacing: 0) {
                        fileHeader(file)
                        Divider().opacity(0.65)
                        if loadingPatch {
                            GitHubSkeleton(rows: 2)
                        } else if let patchError {
                            Text(patchError).font(.callout).foregroundStyle(.red).textSelection(.enabled).padding(24)
                        } else if displayPatch(file).isEmpty {
                            VStack(spacing: 8) {
                                Image(nsImage: GitHubFileIcon.image(fileName: file["filename"].text)).resizable().scaledToFit().frame(width: 30, height: 30)
                                Text(GHL("No text changes are available for this file.")).foregroundStyle(.secondary)
                            }.frame(maxWidth: .infinity).padding(32)
                        } else {
                            GitHubNativeDiff(patch: displayPatch(file), fileName: file["filename"].text, layout: layout) { contentHeight = $0 }
                                .frame(height: min(max(66, contentHeight + 8), max(66, geometry.size.height - 74)))
                                .frame(maxWidth: .infinity)
                        }
                    }
                    .background(Color(nsColor: .textBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.1)) }
                    .padding(16)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                } else if service.sections[.files]?.loading == true {
                    GitHubSkeleton(rows: 3)
                } else {
                    Text(GHL("No changed files.")).foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }.frame(minWidth: 260, maxWidth: .infinity, maxHeight: .infinity)
        }
        .task(id: selected?["filename"].text) {
            let request = UUID(); patchRequest = request
            contentHeight = 0; fullPatch = nil; patchError = nil; loadingPatch = false
            guard let file = selected, file["patch"].text.isEmpty else { return }
            loadingPatch = true
            defer { if patchRequest == request { loadingPatch = false } }
            do {
                let raw = try await service.pullRequestDiff(), path = file["filename"].text
                let parsed = await Task.detached(priority: .userInitiated) { GitHubDiffDocument.parse(raw).patch(fileName: path) }.value
                try Task.checkCancellation()
                guard patchRequest == request else { return }
                fullPatch = parsed
            } catch is CancellationError {} catch { if patchRequest == request { patchError = error.localizedDescription } }
        }
    }
    private func displayPatch(_ file: GitHubValue) -> String { file["patch"].string ?? fullPatch ?? "" }
    private func fileHeader(_ file: GitHubValue) -> some View {
        HStack(spacing: 9) {
            Text(file["filename"].text).font(.system(size: 12, weight: .medium, design: .monospaced))
                .lineLimit(1).truncationMode(.middle).textSelection(.enabled)
            Spacer(minLength: 8)
            GitHubChangeCount(item: file)
            Divider().frame(height: 15).padding(.horizontal, 3)
            layoutButton(.unified, title: GHL("Unified diff"), symbol: "rectangle")
            layoutButton(.split, title: GHL("Split diff"), symbol: "rectangle.split.2x1")
            Button {
                NSPasteboard.general.clearContents(); NSPasteboard.general.setString(displayPatch(file), forType: .string)
            } label: { Image(systemName: "doc.on.doc").frame(width: 24, height: 24) }
                .buttonStyle(.plain).foregroundStyle(.secondary).help(GHL("Copy diff"))
        }
        .padding(.horizontal, 12).frame(height: 40).background(Color.primary.opacity(0.025))
    }
    private func layoutButton(_ value: GitHubDiffViewController.Layout, title: String, symbol: String) -> some View {
        Button { layout = value } label: {
            Image(systemName: symbol).frame(width: 25, height: 24)
                .background(layout == value ? Color.primary.opacity(0.08) : .clear, in: RoundedRectangle(cornerRadius: 4))
        }.buttonStyle(.plain).foregroundStyle(layout == value ? .primary : .secondary)
            .help(title).accessibilityLabel(title).accessibilityAddTraits(layout == value ? .isSelected : [])
    }
}

struct GitHubNativeDiff: NSViewControllerRepresentable {
    let patch: String
    let fileName: String
    let layout: GitHubDiffViewController.Layout
    var onContentHeightChange: @MainActor (CGFloat) -> Void = { _ in }

    @MainActor final class Coordinator {
        var report: @MainActor (CGFloat) -> Void = { _ in }
        var active = true
        var height: CGFloat?
        func reportHeight(_ value: CGFloat) {
            guard value != height else { return }
            height = value
            Task { @MainActor [weak self] in
                guard let self, active, height == value else { return }
                report(value)
            }
        }
    }
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeNSViewController(context: Context) -> GitHubDiffViewController {
        let controller = GitHubDiffViewController()
        context.coordinator.report = onContentHeightChange
        controller.onContentHeightChange = { [weak coordinator = context.coordinator] in coordinator?.reportHeight($0) }
        controller.loadViewIfNeeded()
        return controller
    }
    func updateNSViewController(_ controller: GitHubDiffViewController, context: Context) {
        context.coordinator.report = onContentHeightChange
        controller.configure(patch: patch, layout: layout, showsFileHeaders: false, fileName: fileName)
    }
    static func dismantleNSViewController(_ controller: GitHubDiffViewController, coordinator: Coordinator) {
        coordinator.active = false; controller.onContentHeightChange = nil; controller.invalidate()
    }
}
