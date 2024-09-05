import express from 'express';
import cors from 'cors';
import { createServer } from 'http';
import ws from 'ws';
import { createAPI } from './api/api';
import { randomBytes } from 'crypto';

const SERVER_PORT = 3000;
const AUTH_SECRET =
    process.env.AUTH_SECRET ?? (process.env.NODE_ENV === 'production' ? randomBytes(64).toString('hex') : 'dev');

const app = express();
const apiRouter = createAPI({}, AUTH_SECRET);
const httpServer = createServer(app);
const wss = new ws.Server({ server: httpServer });

httpServer.listen(SERVER_PORT, () => console.log(`Server started on port ${SERVER_PORT}`));

app.use(cors());
app.use(express.json());
app.use('/api', apiRouter.getRouter());

app.get('/', (req, res) => {
    res.send('Hello World!');
});

wss.on('connection', ws => {
    console.log('new connection');
    ws.on('message', message => {
        console.log('received: %s', message);
        ws.send(`echo: ${message}`);
    });
});
