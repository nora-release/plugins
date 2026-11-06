import SwiftUI

struct GitHubAccountHeader<Avatar: View, Actions: View>: View {
    let login: String
    @ViewBuilder let avatar: Avatar
    @ViewBuilder let actions: Actions

    var body: some View {
        HStack(spacing: 9) {
            avatar.frame(width: 24, height: 24).clipShape(Circle())
            Menu {
                actions
            } label: {
                HStack(spacing: 8) {
                    Text(login).fontWeight(.semibold).lineLimit(1)
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.down").font(.system(size: 10)).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 28, maxHeight: 28)
                .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden)
            .frame(height: 28)
            .accessibilityIdentifier("github.account.menu")
        }
        .padding(.horizontal, 12)
        .frame(height: 48)
        .accessibilityIdentifier("github.account")
    }
}
