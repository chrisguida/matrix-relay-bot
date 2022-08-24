use std::{
    collections::{BTreeMap, HashMap},
    env,
    process::exit,
};

use matrix_sdk::{
    self,
    config::SyncSettings,
    room::Room,
    ruma::{
        api::client::membership::join_room_by_id_or_alias,
        events::room::{
            member::StrippedRoomMemberEvent,
            message::{
                MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
                TextMessageEventContent,
            },
        },
        uint, OwnedRoomId,
    },
    Client, Error,
};
use tokio::time::{sleep, Duration};
use url::Url;

async fn link(from_room: Room, to_room: Room, two_way: bool) {}

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

async fn on_room_message(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    two_way_map: BTreeMap<OwnedRoomId, Room>,
    username: String,
) {
    if !two_way_map.contains_key(room.room_id()) {
        return;
    }
    // self.
    if let Room::Joined(tx_room) = room {
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
            let member = tx_room.get_member(&sender).await.unwrap().unwrap();
            let name = member
                .display_name()
                .unwrap_or_else(|| member.user_id().as_str());

            if name != username {
                if msg_body.starts_with("!") {
                    let command = &msg_body[1..];
                    let content = match command {
                        "relay" => {
                            RoomMessageEventContent::text_plain(format!("Linking room: {command}"))
                            // link(room)
                        }
                        _ => RoomMessageEventContent::text_plain(format!(
                            "Command not found: {command}"
                        )),
                    };
                    // let content = RoomMessageEventContent::text_plain(format!(
                    //     "{name} has entered the {command} command!"
                    // ));
                    tx_room.send(content, None).await.unwrap();
                } else {
                    // println!("sending");
                    let rx_room = two_way_map.get(tx_room.room_id()).unwrap();
                    if let Room::Joined(rx_room) = rx_room {
                        rx_room
                            .send(
                                RoomMessageEventContent::text_plain(format!("{name}: {msg_body}")),
                                None,
                            )
                            .await
                            .unwrap();
                    }
                }
            }
        }
    }
}

async fn get_all_tor_rooms(
    client: &Client,
) -> matrix_sdk::Result<Vec<(OwnedRoomId, String)>, Error> {
    use matrix_sdk::ruma::{api::client::directory::get_public_rooms_filtered, directory::Filter};

    println!("Searching for rooms whose name contains '(Tor)')");

    let mut filter = Filter::new();
    filter.generic_search_term = Some("(Tor)");
    let mut request = get_public_rooms_filtered::v3::Request::new();
    request.filter = filter;

    let response = client.public_rooms_filtered(request).await?;

    let mut tor_room_ids: Vec<(OwnedRoomId, String)> = Vec::new();

    for public_rooms_chunk in response.chunk {
        println!("Found room {:?}", public_rooms_chunk.name);
        tor_room_ids.push((public_rooms_chunk.room_id, public_rooms_chunk.name.unwrap()));
    }

    Ok(tor_room_ids)
}

async fn get_room_id_pairs(
    client: &Client,
    tor_rooms: Vec<(OwnedRoomId, String)>,
) -> matrix_sdk::Result<Vec<(OwnedRoomId, OwnedRoomId)>, Error> {
    use matrix_sdk::ruma::{api::client::directory::get_public_rooms_filtered, directory::Filter};

    let mut corresponding_room_ids: Vec<(OwnedRoomId, OwnedRoomId)> = Vec::new();

    for (room_id, room_name) in tor_rooms {
        let (clearnet_name, _) = room_name.rsplit_once(' ').unwrap();
        println!("Searching for rooms whose name is '{clearnet_name}'");

        let mut filter = Filter::new();
        filter.generic_search_term = Some(clearnet_name);
        let mut request = get_public_rooms_filtered::v3::Request::new();
        request.filter = filter;

        let response = client.public_rooms_filtered(request).await?;
        println!("Response = {:?}", response);

        if response.next_batch.is_some() || response.chunk.len() > 2 {
            println!(
                "WARN: Found too many rooms ({:?}) with substring {clearnet_name}, skipping",
                response.chunk.len()
            );
        }
        for public_rooms_chunk in response.chunk {
            println!("Found room {:?}", public_rooms_chunk.name);
            if public_rooms_chunk.name.unwrap() != clearnet_name {
                continue;
            } else {
                corresponding_room_ids.push((room_id.clone(), public_rooms_chunk.room_id.clone()));
            }
        }
    }

    Ok(corresponding_room_ids)
}

fn get_two_way_map_from_pairs(
    client: &Client,
    room_id_pairs: Vec<(OwnedRoomId, OwnedRoomId)>,
) -> BTreeMap<OwnedRoomId, Room> {
    let mut two_way_map = BTreeMap::new();
    for (tor_room_id, clearnet_room_id) in room_id_pairs {
        println!("Fetching tor_room = {:?}", &tor_room_id);
        let tor_room = client.get_room(&tor_room_id).unwrap();
        // {
        //     Ok
        // };
        println!("Fetching clearnet_room = {clearnet_room_id}");
        let clearnet_room = client.get_room(&clearnet_room_id).unwrap();
        two_way_map.insert(tor_room_id.clone(), clearnet_room);
        two_way_map.insert(clearnet_room_id.clone(), tor_room);
    }
    two_way_map
}

async fn relay(homeserver_url: String, username: &str, password: &str) -> anyhow::Result<()> {
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

    // get tor <-> clearnet room map and add event handler to relay messages between corresponding room pairs
    let tor_rooms = get_all_tor_rooms(&client).await?;
    println!("Tor rooms = {:?}", tor_rooms);
    let room_id_pairs = get_room_id_pairs(&client, tor_rooms).await?;

    println!("Room ID pairs = {:?}", room_id_pairs);

    let two_way_map = get_two_way_map_from_pairs(&client, room_id_pairs);

    println!("Two way map = {:?}", two_way_map);

    // now that we've synced, let's attach a handler for incoming room messages, so
    // we can react on it
    // client.add_event_handler(on_room_message);
    let username = username.to_owned();
    client.add_event_handler({
        let username = username.clone();
        // let room_id_pairs = room_id_pairs.clone();
        move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let username = username.clone();
            let two_way_map = two_way_map.clone();
            async move {
                on_room_message(ev, room, two_way_map, username).await;
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
