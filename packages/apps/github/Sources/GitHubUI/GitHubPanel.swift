import GitHubCore
import SwiftUI

public struct GitHubPanel: View {
    @Bindable private var service: GitHubService
    @State private var showsConnection = false
    @State private var showsMerge = false
    @State private var commentDrafts: [String: String] = [:]
    @State private var filter = GitHubBoardFilter.all
    /// The item the drawer shows. The drawer opens and closes with it, animated, ahead of the service:
    /// nothing else changes while it slides. The service loads the details once the drawer is in place,
    /// and forgets them once it has gone.
    @State private var drawerItem: GitHubValue?
    /// The drawer's full details appear once it has slid in; drawing them on its first frame would stall the slide.
    @State private var drawerReady = false
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(service: GitHubService) { self.service = service }

    public var body: some View {
        Group {
            if service.user == nil { connection }
            else {
                ZStack(alignment: .trailing) {
                    GitHubBoardView(service: service, filter: $filter, searchFocused: $searchFocused,
                                    selectedKey: drawerItem?.itemKey,
                                    reconnect: { showsConnection = true }, open: open, handOff: handOff)
                    if let item = drawerItem {
                        GitHubBoardStyle.scrim
                            .ignoresSafeArea()
                            .onTapGesture(perform: closeDetails)
                            .transition(.opacity)
                            .accessibilityHidden(true)
                        drawer(service.detail ?? service.selectedItem ?? item)
                            .transition(.move(edge: .trailing))
                    }
                }
                .onChange(of: service.selectedItem?.itemKey) { _, key in follow(key) }
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
        .environment(\.githubImages, service.images)
        .environment(\.openURL, OpenURLAction { url in
            Task { try? await service.openURL(url) }
            return .handled
        })
        .accessibilityIdentifier("github.panel")
    }

    private func drawer(_ item: GitHubValue) -> some View {
        Group {
            if drawerReady {
                GitHubItemDetailView(service: service, item: item, drafts: $commentDrafts)
                    .id(item.itemKey)
                    .transition(.opacity)
            } else {
                GitHubDrawerPlaceholder(item: item)
            }
        }
        .frame(width: GitHubBoardStyle.drawerWidth)
        .frame(maxHeight: .infinity)
        .background(GitHubBoardStyle.drawer)
        .overlay(alignment: .leading) { Rectangle().fill(GitHubBoardStyle.divider).frame(width: 1) }
        // A fixed band of shade, not a blurred shadow: a shadow over the whole drawer is redrawn on every
        // frame of the slide.
        .background(alignment: .leading) {
            LinearGradient(colors: [.clear, .black.opacity(0.16)], startPoint: .leading, endPoint: .trailing)
                .frame(width: 18).offset(x: -18)
                .allowsHitTesting(false)
        }
        .accessibilityIdentifier("github.drawer")
    }

    private var slideDuration: Duration { reduceMotion ? .zero : .milliseconds(280) }
    private var slide: Animation? { reduceMotion ? nil : .smooth(duration: 0.28) }

    /// Slides the drawer in with the card's title at once; once it is in place, loads and shows the details.
    private func open(_ item: GitHubValue) {
        let opening = present(item)
        let key = item.itemKey
        Task {
            if opening { try? await Task.sleep(for: slideDuration) }
            guard drawerItem?.itemKey == key else { return }
            await service.selectItem(item)
        }
    }

    /// Shows the item in the drawer, sliding it in when closed; says whether it slid in.
    @discardableResult
    private func present(_ item: GitHubValue) -> Bool {
        guard drawerItem?.itemKey != item.itemKey else { return false }
        let opening = drawerItem == nil
        drawerReady = !opening
        withAnimation(slide) { drawerItem = item }
        guard opening else { return false }
        let key = item.itemKey
        Task {
            try? await Task.sleep(for: slideDuration)
            guard drawerItem?.itemKey == key else { return }
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.16)) { drawerReady = true }
        }
        return true
    }

    /// Follows details opened or closed elsewhere: Escape, Nora's toolbar, or handing a card to Nora.
    private func follow(_ key: String?) {
        if let item = service.selectedItem, key != nil { present(item) }
        else if key == nil, drawerItem != nil { withAnimation(slide) { drawerItem = nil } }
    }

    /// Slides the drawer out; the service forgets the details once it has gone.
    private func closeDetails() {
        withAnimation(slide) { drawerItem = nil }
        Task {
            try? await Task.sleep(for: slideDuration)
            guard drawerItem == nil else { return }
            await service.performPanelAction("closeDetails")
        }
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

/// What the drawer shows while it slides in: the card's title and state, then rows to come.
private struct GitHubDrawerPlaceholder: View {
    let item: GitHubValue

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(item.title).font(.system(size: 18, weight: .semibold)).lineLimit(3)
            GitHubStatePill(item: item)
            // Still rows: a shimmer here would be one more animation on top of the slide.
            VStack(alignment: .leading, spacing: 12) {
                ForEach(0 ..< 4, id: \.self) { row in
                    RoundedRectangle(cornerRadius: 5).fill(Color.primary.opacity(0.06))
                        .frame(maxWidth: row == 3 ? 260 : .infinity).frame(height: row == 0 ? 72 : 14)
                }
            }
            .padding(.top, 10)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 24).padding(.top, 22)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityElement(children: .combine)
    }
}
