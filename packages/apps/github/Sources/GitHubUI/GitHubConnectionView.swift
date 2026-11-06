import AppKit
import GitHubCore
import SwiftUI

struct GitHubConnectionView: View {
    @Bindable var service: GitHubService
    @State private var showsToken = false
    @State private var token = ""
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text(GHL("Connect GitHub")).font(.title2.weight(.semibold))
            Text(GHL("Sign in to see the issues and pull requests that need your attention."))
                .foregroundStyle(.secondary)
            if let authorization = service.deviceAuthorization {
                VStack(alignment: .leading, spacing: 12) {
                    Text(GHL("Enter this code on GitHub:")).font(.callout)
                    HStack {
                        Text(authorization.userCode).font(.system(size: 24, weight: .medium, design: .monospaced)).textSelection(.enabled)
                        Spacer()
                        Button {
                            NSPasteboard.general.clearContents(); NSPasteboard.general.setString(authorization.userCode, forType: .string)
                        } label: { Image(systemName: "doc.on.doc") }.buttonStyle(.borderless).help(GHL("Copy code"))
                    }
                    Button(GHL("Open GitHub")) { Task { try? await service.openURL(authorization.verificationURL) } }
                }.padding(16).background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 6))
            } else {
                Button {
                    Task { await service.connect() }
                } label: {
                    HStack {
                        if service.isConnecting { ProgressView().controlSize(.mini) }
                        Text(GHL("Continue with GitHub"))
                    }.frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent).controlSize(.large).disabled(service.isConnecting)
            }
            DisclosureGroup(GHL("Use a personal access token"), isExpanded: $showsToken) {
                VStack(alignment: .leading, spacing: 10) {
                    SecureField(GHL("GitHub token"), text: $token).textFieldStyle(.roundedBorder)
                    Button(GHL("Connect")) {
                        Task { await service.connectToken(token); if service.user != nil { token = ""; dismiss() } }
                    }.disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }.padding(.top, 10)
            }.font(.system(size: 12))
            if let error = service.error { Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
            HStack {
                Button(GHL("Manage repository access")) { Task { await service.manageAccess() } }.buttonStyle(.link)
                Spacer()
                Button(GHL("Done")) { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(width: 420)
        .onChange(of: service.user) { _, user in if user != nil { dismiss() } }
    }
}
