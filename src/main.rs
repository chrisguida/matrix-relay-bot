use std::{env, process::exit};

use matrix_sdk::{
    self,
    config::SyncSettings,
    room::Room,
    ruma::events::room::{
        member::StrippedRoomMemberEvent,
        message::{
            MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
            TextMessageEventContent,
        },
    },
    Client,
};
use tokio::time::{sleep, Duration};
use url::Url;

// Whenever we see a new stripped room member event, we've asked our client to
// call this function. So what exactly are we doing then?
async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    client: Client,
    room: Room,
) {
    if room_member.state_key != client.user_id().unwrap() {
        // the invite we've seen isn't for us, but for someone else. ignore
        return;
    }

    // looks like the room is an invited room, let's attempt to join then
    if let Room::Invited(room) = room {
        println!("Autojoining room {}", room.room_id());
        let mut delay = 2;

        while let Err(err) = room.accept_invitation().await {
            // retry autojoin due to synapse sending invites, before the
            // invited user can join for more information see
            // https://github.com/matrix-org/synapse/issues/4345
            eprintln!(
                "Failed to join room {} ({err:?}), retrying in {delay}s",
                room.room_id()
            );

            sleep(Duration::from_secs(delay)).await;
            delay *= 2;

            if delay > 3600 {
                eprintln!("Can't join room {} ({err:?})", room.room_id());
                break;
            }
        }
        println!("Successfully joined room {}", room.room_id());
    }
}

async fn on_room_message(event: OriginalSyncRoomMessageEvent, room: Room, username: String) {
    // self.
    if let Room::Joined(room) = room {
        if let OriginalSyncRoomMessageEvent {
            content:
                RoomMessageEventContent {
                    msgtype: MessageType::Text(TextMessageEventContent { body: msg_body, .. }),
                    ..
                },
            sender,
            ..
        } = event
        {
            let member = room.get_member(&sender).await.unwrap().unwrap();
            let name = member
                .display_name()
                .unwrap_or_else(|| member.user_id().as_str());

            if name != username {
                let content = RoomMessageEventContent::text_plain(format!("{name}: {msg_body}"));
                println!("sending");
                room.send(content, None).await.unwrap();
            }
        }
    }
}

async fn relay(homeserver_url: String, username: &str, password: &str) -> matrix_sdk::Result<()> {
    let homeserver_url = Url::parse(&homeserver_url).expect("Couldn't parse the homeserver URL");
    let client = Client::new(homeserver_url).await.unwrap();

    client
        .login_username(username, password)
        .initial_device_display_name("matrix-relay-bot")
        .send()
        .await?;

    println!("logged in as {username}");

    // Now, we want our client to react to invites. Invites sent us stripped member
    // state events so we want to react to them. We add the event handler before
    // the sync, so this happens also for older messages. All rooms we've
    // already entered won't have stripped states anymore and thus won't fire
    client.add_event_handler(on_stripped_state_member);

    // An initial sync to set up state and so our bot doesn't respond to old
    // messages. If the `StateStore` finds saved state in the location given the
    // initial sync will be skipped in favor of loading state from the store
    client.sync_once(SyncSettings::default()).await.unwrap();

    // now that we've synced, let's attach a handler for incoming room messages, so
    // we can react on it
    // client.add_event_handler(on_room_message);
    let username = username.to_owned();
    client.add_event_handler({
        let username = username.clone();
        move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let username = username.clone();
            async move {
                on_room_message(ev, room, username).await;
            }
        }
    });

    // since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`
    let settings = SyncSettings::default().token(client.sync_token().await.unwrap());
    // this keeps state from the server streaming in to the bot via the
    // EventHandler trait
    client.sync(settings).await; // this essentially loops until we kill the bot

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let (homeserver_url, username, password) =
        match (env::args().nth(1), env::args().nth(2), env::args().nth(3)) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => {
                eprintln!(
                    "Usage: {} <homeserver_url> <username> <password>",
                    env::args().next().unwrap()
                );
                exit(1)
            }
        };

    relay(homeserver_url, &username, &password).await?;

    Ok(())
}
