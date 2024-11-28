# Kalatori docs

!!!OUTDATED!!!


### Decisions
1. Amounts in tx/rx is f64, daemon stores it as u128

f64 amount is rounded up in frontend; down in backend - to make sure order always passes

Reasoning:

- JSON messes up long integers routinely (no exact implementation is defined)
- nobody cares about significant figures outside of u64 precision
- this supports a good natured philosophy of friendly haggling and discounts that's natural for good business

# Daemon API

## Overview

API for interacting with the daemon. 

## Endpoints

### Health Check

- **Endpoint**: `GET /health`
- **Description**: Returns the status of the API and its current version.

- **Response (200 OK)**:
    ```json
    {
        "version": string
        "chain": string,
        "code": string,
    }
    ```

### Create or get Account if it already exists

- **Endpoint**: `POST /account`
- **Description**: Creates a new account, returns account if exists already

- **Request**:
    ```json
    {
        "order_id": string, 
        "amount": float,
    }
    ```

- **Response (201 Created/200 OK)**:
    ```json
    {
        "status": string:pending|paid,
        "address": string,
        "order_id": string,
        "rpc": string,
        "decimals": integer,
        "amount": float
    }
    ```

- **Response(400 Bad Request)**: 
    ```json
    {
        "error": string
    }
    ```
    

## Common Responses for Errors

- **404 Not Found**:
    - Empty body

- **500 Internal Server Error**:
    ```json
    {
        "error": string
    }
    ```


