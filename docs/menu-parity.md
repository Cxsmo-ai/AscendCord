# Context menu parity with Discord

Checked menu by menu against Discord's desktop client. **Present** existed before this
pass, **Added** is new, **Omitted** lists what is deliberately not offered and why.
Items marked *(Dev)* appear only with Settings › General › Advanced › Developer Mode.

## Messages (right click or the ⋯ button)

| Discord item | Status | Notes |
|---|---|---|
| Quick reactions row | Added | The four most used emoji (Discord's defaults until there is history); only emoji the reader may add here are shown. Choosing one toggles the reaction. |
| Add Reaction | Added | Opens the reaction picker anchored on the menu item (the hover toolbar button already existed). |
| Edit Message | Present | Own messages; renamed from "Edit message". |
| Reply | Present | |
| Forward | Present | |
| Create Thread | Present | Renamed from "Create Thread…". |
| Copy Text | Present | Renamed from "Copy message". "Copy" for a text selection is kept. |
| Pin / Unpin Message | Present | Title Case. |
| View Reactions | Present | |
| Mark Unread | Present | |
| Mark Read Through Here | Present | Client extra, kept. |
| Copy Message Link | Added | `https://discord.com/channels/{server or @me}/{channel}/{message}`. |
| Delete Message | Present | Shown only when allowed (own messages or Manage Messages), in red. |
| Copy Message ID *(Dev)* | Added | |
| Apps › | Omitted | Needs the user-installed app command runtime. |
| Speak Message | Omitted | There is no text-to-speech engine in the client yet. |
| Report Message | Omitted | Not part of the documented API. |
| Remove All Reactions | Omitted (for now) | Needs a moderator reaction transport; tracked as a follow-up. |

## Users (member list, DMs, avatars, voice participants)

| Discord item | Status | Notes |
|---|---|---|
| Profile | Present | |
| Mention | Present | Only where text can be sent. |
| Message | Added | Opens the existing DM, or starts one with a friend. |
| Add Note | Present | |
| Add / Edit Friend Nickname | Present | |
| Pin / Unpin DM | Present | DM rows only. |
| Mute / Unmute Conversation, Close DM | Present | |
| Add Friend / Remove Friend | Added | Remove uses the existing confirmation dialog. |
| Block / Unblock | Present | |
| Copy User ID *(Dev)* | Added | Also for your own account and webhooks, as in Discord. Member settings' Copy User ID is now Developer Mode too. |
| Invite to Server › | Omitted (for now) | Invites are sent from the server's Invite People dialog. |
| Call | Omitted | Starting DM calls from the menu is not wired yet. |

## Channels

| Discord item | Status | Notes |
|---|---|---|
| Mark As Read, Add To Favorites, Invite to Channel, Copy Link, Mute Channel ›, Notification Settings ›, Edit / Duplicate / Create / Delete Channel | Present | |
| Copy Channel ID *(Dev)* | Present | Now behind Developer Mode. |

## Servers (server icon and server header)

| Discord item | Status | Notes |
|---|---|---|
| Mark As Read | Present | |
| Invite People | Added | On the server icon menu; renamed from "Create invite" in the header. |
| Server Settings | Present | |
| Hide Muted Channels, Create Channel, Create Category | Present | In the channel list's own menu. |
| Leave Server | Present | Renamed from "Leave server"; hidden for the owner. |
| Copy Server ID *(Dev)* | Added | Both menus. |
| Mute Server ›, Notification Settings › (server level) | Omitted (for now) | Needs the server-level notification settings write; channel-level settings exist. |
| Privacy Settings, Edit Server Profile | Omitted (for now) | |

## Voice participants

| Discord item | Status | Notes |
|---|---|---|
| Mute (local), User Volume, Reset Volume | Present | Local only. |
| Server Mute / Unmute | Added | Needs Mute Members in that channel. |
| Server Deafen / Undeafen | Added | Needs Deafen Members in that channel. |
| Move To › | Added | Needs Move Members, and Connect to the destination for you (the moderator). Voice and stage channels of the same server. |
| Disconnect | Added | Needs Move Members. Sends `channel_id: null`. |
| User items (Profile, Message, …) | Present | Shared with the user menu. |

Voice moderation does not follow role hierarchy (Discord only applies hierarchy to role,
nickname, kick and ban actions). It is one `PATCH /guilds/{guild}/members/{user}` with
only the changed field; a returned member must show the requested `mute`/`deaf` state.
The voice roster itself updates from the gateway.

## Message composer

| Discord item | Status | Notes |
|---|---|---|
| Cut, Copy, Paste, Select All | Present | "Select all" renamed to Discord's "Select All". |
