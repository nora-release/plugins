import ExtensionFoundation
import ExtensionKit
import Foundation
import GitHubCore
import GitHubUI
import NoraPluginKit
import SwiftUI

@main @MainActor
final class GitHubPlugin: NSObject, AppExtension {
    private let credentialCoordinator: GitHubCredentialCoordinator
    private let commands: GitHubService
    override required init() {
        let coordinator = GitHubCredentialCoordinator()
        credentialCoordinator = coordinator
        commands = GitHubService(credentialCoordinator: coordinator)
        super.init()
    }

    var configuration: AppExtensionSceneConfiguration {
        let commands = commands
        let commandService = NoraPluginService { request, host in
            await Self.configure(commands, host: host)
            return try await Self.invokeCommand(request, model: commands)
        }
        return AppExtensionSceneConfiguration(self.makePanelScene(), configuration: commandService)
    }

    private func makePanelScene() -> PrimitiveAppExtensionScene {
        let runtime = GitHubPanelRuntime(credentialCoordinator: credentialCoordinator)
        let service = NoraPluginService(handler: { request, _ in
            guard request.method == NoraPluginPanelPresentation.actionMethod else { return NoraPluginResponse(error: "Unsupported GitHub panel request.") }
            let action = try request.parameters(as: NoraPluginPanelActionInvocation.self)
            guard action.panelID == GitHubPanelRuntime.panelID else { throw NoraPluginConnectionError.unavailable }
            let handled = await runtime.model.performPanelAction(action.actionID)
            return try NoraPluginResponse(result: handled)
        }, onConnect: { host, context in
            guard context.panelID == GitHubPanelRuntime.panelID else { return }
            Task { @MainActor in await runtime.connect(host) }
        })
        return PrimitiveAppExtensionScene(id: GitHubPanelRuntime.panelID,
            content: { GitHubPanel(service: runtime.model) }, onConnection: { service.accept(connection: $0) })
    }

    fileprivate static func configure(_ model: GitHubService, host: NoraPluginHostClient) {
        model.configure { method, parameters in
            try await host.request(method: method, parameters: parameters, as: GitHubValue.self)
        }
    }

    private static func invokeCommand(_ request: NoraPluginRequest, model: GitHubService) async throws -> NoraPluginResponse {
        guard request.method == "commands.execute" else { return NoraPluginResponse(error: "Unsupported GitHub plugin request.") }
        let input = try request.parameters(as: CommandInput.self)
        let page = input.arguments["page"].int ?? 1
        let repository = input.arguments["repository"].text
        let stateString = input.arguments["state"].string ?? "open"
        guard let state = GitHubItemState(rawValue: stateString) else { throw GitHubAPIError.invalidResponse }
        let api = try await model.apiForCommands()
        let result: GitHubPage
        switch input.commandID {
        case "com.nora.github.listRepositories": result = try await api.repositories(page: page)
        case "com.nora.github.listIssues": result = try await api.issues(repository: repository, state: state, page: page)
        case "com.nora.github.listPullRequests": result = try await api.pullRequests(repository: repository, state: state, page: page)
        default: return NoraPluginResponse(error: "Unsupported GitHub plugin command.")
        }
        return try NoraPluginResponse(result: result)
    }
}

@MainActor
private final class GitHubPanelRuntime {
    nonisolated static let panelID = "com.nora.github.panel"
    let model: GitHubService
    init(credentialCoordinator: GitHubCredentialCoordinator) { model = GitHubService(credentialCoordinator: credentialCoordinator) }
    private var host: NoraPluginHostClient?

    func connect(_ host: NoraPluginHostClient) async {
        self.host = host
        GitHubPlugin.configure(model, host: host)
        model.onPresentationChange = { [weak self] in await self?.present() }
        await model.activate()
    }

    private func present() async {
        guard let host else { return }
        let item = model.detail
        let status: NoraPluginPanelStatus?
        if let item {
            let merged = item["merged"].bool == true || !item["merged_at"].isNull
            let closed = item["state"].text == "closed", draft = item["draft"].bool == true
            status = NoraPluginPanelStatus(title: merged ? GitHubLocalization.text("Merged") : closed ? GitHubLocalization.text("Closed") : draft ? GitHubLocalization.text("Draft") : GitHubLocalization.text("Open"),
                systemImage: merged ? "arrow.triangle.merge" : model.kind == .issues ? "questionmark.bubble" : "arrow.triangle.pull",
                tone: merged ? .purple : closed ? .danger : draft ? .muted : .success, iconSVG: GitHubNativeIcon.svg(for: item))
        } else { status = nil }
        var actions: [NoraPluginPanelAction] = []
        if item != nil {
            actions.append(NoraPluginPanelAction(id: "handoff", title: GitHubLocalization.text("Give to Nora"), systemImage: "sparkles", isPrimary: true))
            if model.kind == .pullRequests, item?["state"].text == "open", item?["draft"].bool != true,
               model.sections[.repository]?.data["permissions"]["push"].bool == true {
                actions.append(NoraPluginPanelAction(id: "merge", title: GitHubLocalization.text("Merge"), systemImage: "arrow.triangle.merge", isEnabled: !model.isMerging, iconSVG: GitHubNativeIcon.mergeSVG))
            }
        }
        actions.append(NoraPluginPanelAction(id: "refresh", title: GitHubLocalization.text("Refresh"), systemImage: "arrow.clockwise", isEnabled: !model.isLoadingDetail && !model.isLoadingFeed))
        do {
            try await host.present(NoraPluginPanelPresentation(panelID: Self.panelID, title: item?["title"].string,
                number: item?["number"].int, status: status, sidebarVisible: model.sidebarVisible, sidebarWidth: model.sidebarWidth, actions: actions))
        } catch {}
    }
}

private struct CommandInput: Decodable {
    let commandID: String
    let arguments: GitHubValue
}
