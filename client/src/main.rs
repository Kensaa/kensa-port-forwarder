use clap::{Args, Parser, Subcommand};
use dialoguer::theme::ColorfulTheme;
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use ssh_key::{PrivateKey, PublicKey};
use std::{
    cell::RefCell,
    fs,
    io::Write,
    net::TcpStream,
    path::PathBuf,
    process::{self, Stdio},
    rc::Rc,
};
use tungstenite::{self, stream::MaybeTlsStream, Message, WebSocket};
use url::Url;
use uuid::Uuid;

#[cfg(debug_assertions)]
const DEFAULT_SERVER_URL: &str = "localhost:7856";
#[cfg(not(debug_assertions))]
const DEFAULT_SERVER_URL: &str = "port.kensa.fr";

#[derive(Parser, Debug)]
#[command(name = "kensa port forwarder client")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug)]
struct CommonArgs {
    #[arg(
        short,
        long,
        default_value = DEFAULT_SERVER_URL,
        help = "The url of the server to connect to",
        value_parser = |s: &str| -> Result<String,String> {
            let mut server_url = s.to_string();
            if server_url.starts_with("http://") {
                server_url = server_url.replace("http://", "ws://");
            }

            if server_url.starts_with("https://") {
                server_url = server_url.replace("https://", "wss://");
            }

            if !(server_url.starts_with("ws://") || server_url.starts_with("wss://")) {
                if cfg!(debug_assertions) {
                    server_url = format!("ws://{}", server_url);
                } else {
                    server_url = format!("wss://{}", server_url);
                }
            }
            Ok(server_url)
        }
    )]
    server_url: Option<String>,

    #[arg(
        long,
        default_value = "$HOME/.ssh/id_rsa",
        help = "The path to the ssh key to use for the connection",
        value_parser = |s: &str| -> Result<String, String> {
            let mut ssh_key = s.to_string();
            ssh_key = ssh_key.replace(
                "$HOME",
                UserDirs::new().unwrap().home_dir().to_str().unwrap(),
            );
            let priv_key = PathBuf::from(&ssh_key);
            let pub_key = PathBuf::from(ssh_key.clone() + ".pub");

            if !priv_key.exists() {
                return Err(format!(
                    "The ssh private key file \"{}\" does not exist",
                    priv_key.display()
                ));
            }

            if !pub_key.exists() {
                return Err(format!(
                    "The ssh public key file \"{}\" does not exist",
                    pub_key.display()
                ));
            }

            match PrivateKey::read_openssh_file(&priv_key) {
                Ok(key) => key,
                Err(e) => {
                    return Err(format!("the private key is invalid: {}", e));
                }
            };

            match PublicKey::read_openssh_file(&pub_key) {
                Ok(key) => key,
                Err(e) => {
                    return Err(format!("the public key is invalid: {}", e));
                }
            };
            Ok(ssh_key)
        }
    )]
    ssh_key: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Host Command
    #[command()]
    Host(HostArgs),

    /// Connect Command
    #[command()]
    Connect(ConnectArgs),
}

#[derive(Args, Debug)]
struct HostArgs {
    #[arg(long, help = "whether to accept the connection automatically or not")]
    auto_accept: bool,

    #[arg(long, help = "comma serparated list of ports to blacklist")]
    port_blacklist: Option<String>,

    #[arg(long, help = "comma serparated list of ports to whitelist")]
    port_whitelist: Option<String>,

    #[command(flatten)]
    common_args: CommonArgs,
}

#[derive(Args, Debug)]
struct ConnectArgs {
    #[command(flatten)]
    common_args: CommonArgs,

    #[arg(help = "the UUID of the host you want to connect to")]
    target: String,

    #[arg(help = "the port you want to connect to")]
    port: u16,

    #[arg(help = "the port you want to map the port onto")]
    local_port: u16,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ClientType {
    Sender,   // A client which sends a port
    Receiver, // A client which receives a port
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]

enum WSMessage {
    // sent by a Client to register on server
    Register {
        ssh_key: String,
        uuid: String,
        auto_accept: bool,
        port_whitelist: Vec<u16>,
        port_blacklist: Vec<u16>,
        client_type: ClientType,
    },
    // sent by a Receiver to try to connect to a Sender
    ConnectToHost {
        target: String,
        port: u16,
    },
    // sent by the server to a Sender which does not have the auto-accept flag to confirm whether it accept the connection or not
    ConnectConfirm {
        source_client: String,
        port: u16,
    },
    ConnectAccept {},
    ConnectDeny {},
    // response sent by the server to both Sender and Receiver in case of a successful connection
    TunnelConnect {
        client_type: ClientType,
        user: String,        // ssh user
        sshd_port: u16,      // sshd port
        local_port: u16,     // port used to forward between the two clients
        forwarded_port: u16, // port to forward (ignored by receivers)
    },
    TunnelClose {},
    // generic reponse from the server
    Response {
        success: bool,
        error: Option<String>,
    },
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn main() {
    let project_dirs = ProjectDirs::from("fr", "kensa", "kensa-port-forwarder-client").unwrap();
    let data_dir = project_dirs.data_dir();
    if !data_dir.exists() {
        fs::create_dir(data_dir).expect("failed to create folder");
    }
    let id_file = data_dir.join("id");
    // if we are in debug, generate a uuid each time
    let uuid = if id_file.exists() && !cfg!(debug_assertions) {
        fs::read_to_string(id_file).unwrap()
    } else {
        let new_uuid = Uuid::new_v4().to_string();
        let mut file = fs::File::create(id_file).expect("failed to create file");
        file.write(new_uuid.as_bytes()).unwrap();
        new_uuid
    };

    println!("uuid : {}", uuid);

    let cli = Cli::parse();

    match cli.command {
        Command::Host(args) => {
            let server_url = args.common_args.server_url.unwrap();
            let ssh_key_path = args.common_args.ssh_key.unwrap();
            let mut socket = socket_connect(server_url.clone());
            let auto_accept = args.auto_accept;
            let port_blacklist = parse_port_list(args.port_blacklist);
            let port_whitelist = parse_port_list(args.port_whitelist);
            let ssh_key =
                PublicKey::read_openssh_file(&PathBuf::from(ssh_key_path.clone() + ".pub"))
                    .unwrap()
                    .to_string();

            match socket_register(
                &mut socket,
                uuid,
                ssh_key,
                auto_accept,
                port_whitelist,
                port_blacklist,
                ClientType::Sender,
            ) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("{}", err);
                    process::exit(1);
                }
            }

            let running_tunnel: Rc<RefCell<Option<process::Child>>> = Rc::new(RefCell::new(None));
            loop {
                let message = socket_receive(&mut socket);
                println!("{:?}", message);
                match message {
                    WSMessage::ConnectConfirm {
                        source_client,
                        port,
                    } => {
                        let result = dialoguer::Confirm::with_theme(&ColorfulTheme::default())
                            .with_prompt(format!(
                                "Client {} wants to connect to port {}",
                                source_client, port
                            ))
                            .default(true)
                            .interact()
                            .unwrap();

                        if result {
                            socket_send(&mut socket, WSMessage::ConnectAccept {});
                        } else {
                            socket_send(&mut socket, WSMessage::ConnectDeny {});
                        }
                    }
                    WSMessage::TunnelConnect {
                        client_type,
                        user,
                        sshd_port,
                        local_port,
                        forwarded_port,
                    } => {
                        if client_type != ClientType::Sender {
                            eprintln!("the client type received with the tunnel connect message does not match the client, this is a bug with the server");
                            process::exit(1);
                        }
                        let ssh_process = process::Command::new("ssh")
                            .arg("-o")
                            .arg("StrictHostKeyChecking=no")
                            .arg("-N")
                            .arg("-p")
                            .arg(sshd_port.to_string())
                            .arg("-i")
                            .arg(ssh_key_path.clone())
                            .arg("-R")
                            .arg(format!("{}:localhost:{}", local_port, forwarded_port))
                            .arg(format!("{}@{}", user, get_server_domain(&server_url)))
                            // .stderr(Stdio::null())
                            // .stdout(Stdio::null())
                            .spawn()
                            .expect("failed to open ssh tunnel");
                        running_tunnel.borrow_mut().replace(ssh_process);
                    }
                    WSMessage::TunnelClose {} => {
                        if running_tunnel.borrow().is_some() {
                            println!("killing tunnel");
                            running_tunnel
                                .borrow_mut()
                                .take()
                                .unwrap()
                                .kill()
                                .expect("failed to kill tunnel");
                            process::exit(0)
                        }
                    }
                    _ => {}
                }
            }
        }
        Command::Connect(args) => {
            let server_url = args.common_args.server_url.unwrap();
            let mut socket = socket_connect(server_url.clone());
            let port_blacklist = Vec::new();
            let port_whitelist = Vec::new();
            let ssh_key_path = args.common_args.ssh_key.unwrap();
            let ssh_key =
                PublicKey::read_openssh_file(&PathBuf::from(ssh_key_path.clone() + ".pub"))
                    .unwrap()
                    .to_string();

            match socket_register(
                &mut socket,
                uuid,
                ssh_key,
                false,
                port_whitelist,
                port_blacklist,
                ClientType::Receiver,
            ) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("{}", err);
                    process::exit(1);
                }
            }

            let target = args.target;
            let port = args.port;

            let message = WSMessage::ConnectToHost {
                target: target.clone(),
                port,
            };
            socket_send(&mut socket, message);

            let running_tunnel: Rc<RefCell<Option<process::Child>>> = Rc::new(RefCell::new(None));
            loop {
                let message = socket_receive(&mut socket);
                match message {
                    WSMessage::Response { success, error } if !success => {
                        eprintln!("error: {}:\n{}", target, error.unwrap());
                        process::exit(1);
                    }
                    WSMessage::TunnelConnect {
                        client_type,
                        user,
                        sshd_port,
                        local_port,
                        ..
                    } => {
                        if client_type != ClientType::Receiver {
                            eprintln!("the client type received with the tunnel connect message does not match the client, this is a bug with the server");
                            process::exit(1);
                        }
                        let ssh_process = process::Command::new("ssh")
                            .arg("-o")
                            .arg("StrictHostKeyChecking=no")
                            .arg("-N")
                            .arg("-p")
                            .arg(sshd_port.to_string())
                            .arg("-i")
                            .arg(ssh_key_path.clone())
                            .arg("-L")
                            .arg(format!("{}:localhost:{}", args.local_port, local_port))
                            .arg(format!("{}@{}", user, get_server_domain(&server_url)))
                            // .stderr(Stdio::null())
                            // .stdout(Stdio::null())
                            .spawn()
                            .expect("failed to open ssh tunnel");
                        running_tunnel.borrow_mut().replace(ssh_process);
                    }
                    WSMessage::TunnelClose {} => {
                        if running_tunnel.borrow().is_some() {
                            println!("killing tunnel");
                            running_tunnel
                                .borrow_mut()
                                .take()
                                .unwrap()
                                .kill()
                                .expect("failed to kill tunnel");
                            process::exit(0)
                        }
                    }
                    _ => {}
                }
            }

            // let response = socket_receive(&mut socket);
            // println!("{:?}", response);
            // if let WSMessage::Response { success, error } = response {
            //     if !success {
            //         eprintln!(
            //             "failed to connect to the host {}:\n{}",
            //             target,
            //             error.unwrap()
            //         );
            //         process::exit(1);
            //     }
            // } else if let WSMessage::TunnelConnect {
            //     client_type,
            //     user,
            //     sshd_port,
            //     local_port,
            //     forwarded_port,
            // } = response
            // {
            //     println!("Tunnel connect : {:?}, {}, {}", client_type, user, port);
            // }
        }
    }
}

fn parse_port_list(input: Option<String>) -> Vec<u16> {
    match input {
        Some(input) => input
            .split(",")
            .map(|e| e.trim().to_string())
            .filter_map(|p| match p.parse::<u16>() {
                Ok(port) => Some(port),
                Err(_) => None,
            })
            .collect(),
        None => {
            vec![]
        }
    }
}

fn socket_connect(address: String) -> Socket {
    match tungstenite::connect(address) {
        Ok(res) => res.0,
        Err(err) => {
            eprintln!("failed to connect to server \"{}\"", err.to_string());
            process::exit(1);
        }
    }
}

fn socket_register(
    socket: &mut Socket,
    uuid: String,
    ssh_key: String,
    auto_accept: bool,
    port_whitelist: Vec<u16>,
    port_blacklist: Vec<u16>,
    client_type: ClientType,
) -> Result<(), String> {
    let register_message = WSMessage::Register {
        auto_accept,
        port_blacklist,
        port_whitelist,
        uuid,
        ssh_key,
        client_type,
    };
    socket_send(socket, register_message);

    let register_response = socket_receive(socket);
    if let WSMessage::Response { success, error } = register_response {
        if success {
            return Ok(());
        } else {
            return Err(format!(
                "Failed to register with server:\n{}",
                error.unwrap_or("".to_string())
            ));
        }
    }
    return Err("Failed to register with server".to_string());
}

fn socket_receive(socket: &mut Socket) -> WSMessage {
    let msg = socket.read();
    let msg = match msg {
        Ok(msg) => msg,
        Err(_) => {
            eprintln!("an error occurred while reading from socket");
            process::exit(1);
        }
    };
    let msg = msg.into_text().expect("failed to convert message to text");

    let msg: WSMessage =
        serde_json::from_str(&msg).expect("failed to parse message sent by server");

    match &msg {
        WSMessage::Response { success, error } if !success => {
            eprintln!(
                "Server sent an error:\n{}",
                error.as_ref().unwrap_or(&"".to_string())
            );
            process::exit(1);
        }
        _ => {}
    };
    msg
}

fn socket_send(socket: &mut Socket, message: WSMessage) {
    let message = serde_json::to_string(&message).expect("failed to stringify register message");
    socket.send(Message::text(message)).expect("failed to send");
}

fn get_server_domain(url: &str) -> String {
    let url = Url::parse(url).expect("failed to parse server url");
    return url.domain().expect("invalid server_url").to_string();
}
