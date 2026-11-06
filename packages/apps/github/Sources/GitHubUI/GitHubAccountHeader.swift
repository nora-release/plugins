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
                Text(login).fontWeight(.semibold).lineLimit(1)
                    .frame(minHeight: 28, maxHeight: 28)
                    .contentShape(Rectangle())
            }
            // A borderless menu centers its label in the space it is given, so it hugs its label instead.
            .menuStyle(.borderlessButton).menuIndicator(.visible).fixedSize()
            .frame(height: 28)
            .accessibilityIdentifier("github.account.menu")
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(height: 48)
        .accessibilityIdentifier("github.account")
    }
}
