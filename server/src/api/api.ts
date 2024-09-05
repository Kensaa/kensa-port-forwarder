import { APIRouter, createJWTAuthHandler } from '@kensaa/express-api-router';
import z from 'zod';

export interface Instances {}
export interface AuthedUserData {}
export type API = ReturnType<typeof createAPI>;

export function createAPI(instances: Instances, AUTH_SECRET: string) {
    const authHandler = createJWTAuthHandler<AuthedUserData>(AUTH_SECRET);
    const apiRouter = new APIRouter(instances, authHandler);

    return apiRouter;
}
