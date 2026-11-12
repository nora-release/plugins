import GitHubCore
import SwiftUI

public struct GitHubPanel: View {
    @Bindable private var service: GitHubService
    @State private var showsConnection = false
    @State private var showsMerge = false
    @State private var commentDrafts: [String: String] = [:]
    @State private var filter = GitHubBoardFilter.all
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(service: GitHubService) { self.service = service }

    public var body: some View {
        Group {
            if service.user == nil { connection }
            else {
                ZStack(alignment: .trailing) {
                    GitHubBoardView(service: service, filter: $filter, searchFocused: $searchFocused,
                                    reconnect: { showsConnection = true }, handOff: handOff)
                    if let item = service.detail ?? service.selectedItem {
                        GitHubBoardStyle.scrim
                            .ignoresSafeArea()
                            .onTapGesture(perform: closeDetails)
                            .transition(.opacity)
                            .accessibilityHidden(true)
                        drawer(item)
                            .transition(.move(edge: .trailing))
                    }
                }
                .animation(reduceMotion ? nil : .easeOut(duration: 0.22), value: service.selectedItem?.itemKey)
            }
        }
        .font(.system(size: 13))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task { await service.restore() }
        .onAppear {
            service.onUIAction = { action in
                switch action {
                case "focusSearch": searchFocused = true; return true
                case "merge": showsMerge = true; return true
                case "closeDetails":
                    if showsMerge {
                        if !service.isMerging { service.cancelMerge(); showsMerge = false }
                        return true
                    }
                    if showsConnection { showsConnection = false; return true }
                    return false
                default: return false
                }
            }
        }
        .onDisappear { service.onUIAction = nil }
        .onChange(of: service.user?["login"].text) { old, new in
            if old != new { commentDrafts.removeAll() }
        }
        .sheet(isPresented: $showsConnection) { GitHubConnectionView(service: service) }
        .sheet(isPresented: $showsMerge) { GitHubMergeView(service: service) }
        .environment(\.githubAvatars, service.avatars)
        .environment(\.openURL, OpenURLAction { url in
            Task { try? await service.openURL(url) }
            return .handled
        })
        .accessibilityIdentifier("github.panel")
    }

    private func drawer(_ item: GitHubValue) -> some View {
        GitHubItemDetailView(service: service, item: item, drafts: $commentDrafts)
            .id(item.itemKey)
            .frame(width: GitHubBoardStyle.drawerWidth)
            .frame(maxHeight: .infinity)
            .background(GitHubBoardStyle.drawer)
            .overlay(alignment: .leading) { Rectangle().fill(GitHubBoardStyle.divider).frame(width: 1) }
            .shadow(color: .black.opacity(0.22), radius: 24, x: -6)
            .accessibilityIdentifier("github.drawer")
    }

    private func closeDetails() {
        Task { await service.performPanelAction("closeDetails") }
    }

    /// Opens the item so its discussion is read, then hands it to Nora.
    private func handOff(_ item: GitHubValue) {
        Task {
            if service.detail?.itemKey != item.itemKey || service.isLoadingDetail { await service.selectItem(item) }
            await service.handToNora()
        }
    }

    private var connection: some View {
        VStack(spacing: 16) {
            GitHubIcon(kind: .open, size: 38).foregroundStyle(.secondary)
            Text(GHL("GitHub, in Nora")).font(.title2.weight(.semibold))
            Text(GHL("Your issues, reviews, and pull requests in one place."))
                .foregroundStyle(.secondary).multilineTextAlignment(.center)
            Button(GHL("Connect GitHub")) { showsConnection = true }
                .buttonStyle(.borderedProminent).controlSize(.large)
            if let error = service.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
        }.padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(GitHubPanelStyle.background)
    }
}
