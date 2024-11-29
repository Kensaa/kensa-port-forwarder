import { createServer } from 'http';
import ws from 'ws';
import { ZodError } from 'zod';
import { ClientType, messagesSchema } from './schema';
import { ChildProcess, spawn, execSync, spawnSync } from 'child_process';
import fs from 'fs';
import path from 'path';

if (!fs.existsSync('/usr/bin/sshd')) {
    console.error('failed to find sshd');
    process.exit(1);
}
const SERVER_PORT = parseInt(process.env.SERVER_PORT ?? '7856');
const FORWARDING_USER = process.env.FORWARDING_USER;
const OPENED_PORTS = (process.env.OPENED_PORTS ?? '')
    .split(',')
    .map(e => parseInt(e))
    .filter(e => !isNaN(e));

const KEYS_FOLDER = process.env.KEYS_FOLDER ?? 'keys';
const KEYS = ['ssh_host_rsa_key', 'ssh_host_ecdsa_key', 'ssh_host_ed25519_key'].map(key =>
    path.resolve(KEYS_FOLDER, key)
);
if (!fs.existsSync(KEYS_FOLDER)) {
    fs.mkdirSync(KEYS_FOLDER);
}

const tmp_folder = path.join('/tmp', 'authorized_keys');
if (!fs.existsSync(tmp_folder)) {
    fs.mkdirSync(tmp_folder);
    fs.chmodSync(tmp_folder, '700');
}

for (const key of KEYS) {
    if (!fs.existsSync(key)) {
        const keyType = path.parse(key).name.split('_').at(-2)!;
        const args = ['-t', keyType];
        if (keyType == 'rsa') {
            args.push('-b');
            args.push('4096');
        }
        if (keyType == 'ecsda') {
            args.push('-b');
            args.push('521');
        }
        args.push('-f');
        args.push(key);
        // args.push('-N');
        // args.push('""');
        spawnSync('ssh-keygen', args);
        console.log('generated', key);
    }
}

if (!FORWARDING_USER) {
    console.error('please specify the FORWARDING_USER env variable');
    process.exit(1);
} else {
    const userCheck = spawnSync('getent', ['passwd', FORWARDING_USER]);
    if (userCheck.status != 0) {
        console.error(`user ${FORWARDING_USER} does not exists`);
        process.exit(1);
    }
}

if (OPENED_PORTS.length === 0) {
    console.error(
        'please set the OPENED_PORTS env variable to contain a list (comma separated) of opened port for the sshd instances'
    );
    process.exit(1);
}

const httpServer = createServer();
const wss = new ws.Server({ server: httpServer });
httpServer.listen(SERVER_PORT, () => console.log(`Server started on port ${SERVER_PORT}`));

interface Client {
    ws: ws.WebSocket;
    uuid: string;
    ssh_key: string;
    auto_accept: boolean;
    port_whitelist: number[];
    port_blacklist: number[];
    client_type: ClientType;
}

interface Connection {
    sender: Client;
    receiver: Client;
    sshd: ChildProcess;
    sshdPort: number; // port on which this instance of sshd runs
    localPort: number; // port used by both client to push/pull the true port being forwarded from one client to the other
}

const clients: Client[] = [];
const connections: Connection[] = [];

wss.on('connection', ws => {
    ws.on('message', async data => {
        // console.log(data.toString());
        try {
            const message = messagesSchema.parse(JSON.parse(data.toString()));
            if (message.type === 'register') {
                let client = clients.find(c => c.uuid === message.uuid);
                if (client) {
                    client.ws = ws;
                } else {
                    clients.push({ ...message, ws });
                }

                ws.send(
                    JSON.stringify({
                        type: 'response',
                        success: true
                    })
                );
            } else if (message.type === 'connect_to_host') {
                const sourceClient = clients.find(c => c.ws === ws);
                if (!sourceClient) {
                    wsSendResponse(ws, false, 'you are not registered');
                    return;
                }
                const search = clients.filter(c => {
                    if (c.client_type !== 'sender') return false;
                    if (!c.uuid.startsWith(message.target)) return false;
                    return true;
                });

                if (search.length === 0) {
                    wsSendResponse(ws, false, 'There is no client that matches this search');
                    return;
                }
                if (search.length > 1) {
                    wsSendResponse(
                        ws,
                        false,
                        'There are multiples clients that match this search, please be more precise with the uuid provided'
                    );
                    return;
                }
                const targetClient = search[0]!;
                if (targetClient.port_whitelist.length > 0) {
                    // there is a whitelist
                    if (!targetClient.port_whitelist.includes(message.port)) {
                        wsSendResponse(ws, false, `the port "${message.port}" isn't in the client's whitelist`);
                        return false;
                    }
                } else if (targetClient.port_blacklist.length > 0) {
                    //there is a blacklist
                    if (targetClient.port_blacklist.includes(message.port)) {
                        wsSendResponse(ws, false, `the port "${message.port}" is in the client's blacklist`);
                        return false;
                    }
                }

                async function createConnection() {
                    let sshdPort = OPENED_PORTS.find(port => !connections.some(con => con.sshdPort === port));
                    console.log(OPENED_PORTS);
                    if (!sshdPort) {
                        // no port available
                        wsSendResponse(ws, false, 'Server is full');
                        return;
                    }

                    let localPort = Math.max(...OPENED_PORTS) + 1;
                    while (connections.some(con => con.localPort === localPort)) localPort++;
                    // const authorizedKeyArgs = '';
                    const authorizedKeyArgs = `command="echo 'This account is restricted to port forwarding'",no-pty,no-agent-forwarding,no-X11-forwarding`;
                    const sshKeys = [targetClient.ssh_key, sourceClient!.ssh_key]
                        .map(key => authorizedKeyArgs + ' ' + key)
                        .join('\n');

                    const authorizedKeyFile = path.resolve(tmp_folder, `authorized_keys_${sshdPort}`);
                    if (fs.existsSync(authorizedKeyFile)) {
                        fs.rmSync(authorizedKeyFile);
                    }
                    fs.writeFileSync(authorizedKeyFile, `#!/bin/sh\n/bin/echo "${sshKeys}"`);
                    // fs.chmodSync(authorizedKeyFile, 700);

                    const sshdArgs: string[] = [
                        '-f',
                        '/dev/null',
                        '-o',
                        `AllowUsers=${FORWARDING_USER}`,
                        '-o',
                        'PasswordAuthentication=no',
                        '-o',
                        'PubkeyAuthentication=yes',
                        '-o',
                        'AllowTcpForwarding=yes',
                        '-o',
                        'PermitTunnel=no',
                        '-o',
                        'PermitRootLogin=no',
                        '-o',
                        'X11Forwarding=no',
                        '-o',
                        'PermitUserEnvironment=no',
                        '-o',
                        'AllowAgentForwarding=no',
                        '-o',
                        `Port=${sshdPort}`,
                        // '-o',
                        // `MaxSessions=2`,
                        '-o',
                        `PermitOpen=localhost:${localPort}`,
                        '-o',
                        `AuthorizedKeysCommandUser=nobody`,
                        '-o',
                        `AuthorizedKeysCommand=${authorizedKeyFile}`,
                        '-o',
                        `HostKey=${KEYS[0]}`,
                        '-o',
                        `HostKey=${KEYS[1]}`,
                        '-o',
                        `HostKey=${KEYS[2]}`,
                        '-D'
                        // '-o',
                        // 'MaxStartups=2'
                    ];

                    console.log('/usr/bin/sshd ' + sshdArgs.join(' '));

                    const sshd = spawn('/usr/bin/sshd', sshdArgs, {});
                    await wait(1000);
                    let connection: Connection = {
                        sshd,
                        sender: targetClient,
                        receiver: sourceClient!,
                        localPort,
                        sshdPort
                    };
                    connections.push(connection);
                    connection.receiver.ws.send(
                        JSON.stringify({
                            type: 'tunnel_connect',
                            client_type: 'receiver',
                            user: FORWARDING_USER,
                            sshd_port: sshdPort, // ssh port
                            local_port: localPort, // port that is used to forward between the 2 clients
                            forwarded_port: 0 // ignored for receiver
                        })
                    );
                    connection.sender.ws.send(
                        JSON.stringify({
                            type: 'tunnel_connect',
                            client_type: 'sender',
                            user: FORWARDING_USER,
                            sshd_port: sshdPort, // ssh port
                            local_port: localPort, // port that is used to forward between the 2 clients
                            forwarded_port: message.type == 'connect_to_host' ? message.port : 0 // port to forward to local_port
                        })
                    );
                }

                if (targetClient.auto_accept) {
                    createConnection();
                } else {
                    const listener = (data: ws.RawData) => {
                        const message = messagesSchema.parse(JSON.parse(data.toString()));
                        if (message.type === 'connect_accept') {
                            targetClient.ws.removeListener('message', listener);
                            createConnection();
                        } else if (message.type === 'connect_deny') {
                            targetClient.ws.removeListener('message', listener);
                            wsSendResponse(ws, false, 'The client denied the connection');
                        }
                    };
                    targetClient.ws.on('message', listener);
                    targetClient.ws.send(
                        JSON.stringify({
                            type: 'connect_confirm',
                            source_client: sourceClient.uuid,
                            port: message.port
                        })
                    );
                }
            }
        } catch (err) {
            if (err instanceof ZodError) {
                ws.send(
                    JSON.stringify({
                        type: 'response',
                        success: false,
                        error: JSON.stringify(err.errors)
                    })
                );
            } else {
                console.log((err as Error).stack);
                ws.send(
                    JSON.stringify({
                        type: 'response',
                        success: false,
                        error: (err as Error).message
                    })
                );
            }
        }
    });
    ws.on('close', () => {
        let clientIndex = clients.findIndex(c => c.ws === ws);
        if (clientIndex !== -1) {
            const [client] = clients.splice(clientIndex, 1);
            // console.log(`socket ${clients[clientIndex]!.uuid} disconnected`);
            const connectionIndex = connections.findIndex(c => c.sender === client || c.receiver === client);
            if (connectionIndex !== -1) {
                const [connection] = connections.splice(connectionIndex, 1);
                // TODO: close connection
                if (connection!.sender !== client) {
                    connection!.sender.ws.send(
                        JSON.stringify({
                            type: 'tunnel_close'
                        })
                    );
                }
                if (connection!.receiver !== client) {
                    connection!.receiver.ws.send(
                        JSON.stringify({
                            type: 'tunnel_close'
                        })
                    );
                }

                connection!.sshd.kill();
            }
        }
    });
});

async function wait(delay: number) {
    return new Promise(resolve => setTimeout(resolve, delay));
}

function wsSendResponse(ws: ws.WebSocket, success: boolean, error?: string) {
    ws.send(
        JSON.stringify({
            type: 'response',
            success,
            error
        })
    );
}
